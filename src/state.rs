//! Live-state detection for an agent.
//!
//! Combines purely activity-based signals (how recently the agent emitted bytes,
//! how busy the recent window has been) with provider-specific terminal-content
//! heuristics. The provider heuristics are deliberately conservative — they're
//! pattern-matches on rendered output, which means they'll drift when the
//! upstream CLI redesigns its UI. The `[provider]` block below is the only place
//! that should need touching when that happens.

use std::time::{Duration, Instant};

use crate::agent::{Agent, Status};
use crate::config::Provider;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LiveState {
    /// Process spawned recently and hasn't drawn its first frame yet.
    Starting,
    /// Model is actively emitting tokens or running a tool — bytes flowing now.
    Working,
    /// CLI is showing a spinner / "thinking" indicator without much output churn.
    Thinking,
    /// No output for a moment; agent is between bursts.
    Idle,
    /// CLI is parked on a user prompt — your turn.
    Waiting,
    /// Long silence and no detectable prompt; might be hung.
    Stuck,
    /// Child process has exited.
    Exited(i32),
}

impl LiveState {
    pub fn short(&self) -> &'static str {
        match self {
            LiveState::Starting => "starting",
            LiveState::Working => "working",
            LiveState::Thinking => "thinking",
            LiveState::Idle => "idle",
            LiveState::Waiting => "waiting",
            LiveState::Stuck => "stuck",
            LiveState::Exited(_) => "exited",
        }
    }
}

/// Idle thresholds. Tuned so that "waiting" surfaces a few seconds after the last
/// burst — long enough that mid-stream pauses don't flicker the badge.
const ACTIVE_WINDOW: Duration = Duration::from_millis(500);
const IDLE_DEADLINE: Duration = Duration::from_secs(4);
const STUCK_DEADLINE: Duration = Duration::from_secs(45);
const STARTUP_GRACE: Duration = Duration::from_millis(800);

pub fn detect(agent: &Agent) -> LiveState {
    if let Status::Exited(code) = agent.status {
        return LiveState::Exited(code);
    }

    let now = Instant::now();
    if now.duration_since(agent.spawned_at) < STARTUP_GRACE {
        return LiveState::Starting;
    }

    let since_output = now.duration_since(agent.last_output_at);

    // Recent bytes flowing -> Working OR Thinking. Spinner glyphs trigger a lot
    // of small redraws without much meaningful content change, so we lean
    // Thinking when we can see them.
    if since_output < ACTIVE_WINDOW {
        if screen_has_spinner(&agent.parser) {
            return LiveState::Thinking;
        }
        return LiveState::Working;
    }

    // Quiet for a moment. Ask the provider whether the screen looks like a
    // user-input prompt; if so we're Waiting on them.
    if since_output >= IDLE_DEADLINE && provider_awaiting_input(agent.provider, &agent.parser) {
        return LiveState::Waiting;
    }

    if since_output >= STUCK_DEADLINE {
        return LiveState::Stuck;
    }

    LiveState::Idle
}

// ──────────────────────────────────────────────────────────────────────────────
// Heuristics
// ──────────────────────────────────────────────────────────────────────────────

/// Braille spinner glyphs commonly used by Node-based and Rust-based CLI
/// spinners (ora, indicatif, etc). Detecting *any* of them in the bottom rows
/// of the screen is a strong "thinking" signal.
const SPINNER_GLYPHS: &[char] = &[
    '⠋', '⠙', '⠹', '⠸', '⠼', '⠴', '⠦', '⠧', '⠇', '⠏', '◐', '◓', '◑', '◒',
];

fn screen_has_spinner(parser: &vt100::Parser) -> bool {
    let screen = parser.screen();
    let (rows, cols) = screen.size();
    // Only the bottom third — that's where every TUI we care about parks its
    // status / spinner area. Scanning the whole grid would catch spurious uses
    // of these glyphs in chat content.
    let start_row = rows.saturating_sub(rows / 3).max(rows.saturating_sub(8));
    for r in start_row..rows {
        for c in 0..cols {
            if let Some(cell) = screen.cell(r, c) {
                let s = cell.contents();
                if let Some(ch) = s.chars().next()
                    && SPINNER_GLYPHS.contains(&ch)
                {
                    return true;
                }
            }
        }
    }
    false
}

fn provider_awaiting_input(provider: Provider, parser: &vt100::Parser) -> bool {
    match provider {
        Provider::Claude => claude_awaiting_input(parser),
        Provider::Codex => codex_awaiting_input(parser),
        Provider::Gemini => gemini_awaiting_input(parser),
        Provider::Aider => aider_awaiting_input(parser),
        Provider::Shell | Provider::Other => generic_awaiting_input(parser),
    }
}

/// Aider uses a prompt that usually starts with a green `> ` or similar.
/// We match on a line that starts with `> ` near the bottom.
fn aider_awaiting_input(parser: &vt100::Parser) -> bool {
    let screen = parser.screen();
    let (rows, cols) = screen.size();
    for r in rows.saturating_sub(4)..rows {
        let line = row_text(screen, r, cols);
        let trimmed = line.trim_start();
        if trimmed.starts_with("> ") {
            return true;
        }
    }
    false
}

/// Claude Code (`@anthropic-ai/claude-code`) parks on a boxed input area with
/// rounded corners; the cursor sits one row above a `╰─` corner glyph and the
/// previous line starts with `│ >`. We accept either signature so we still
/// detect when the layout shifts slightly.
fn claude_awaiting_input(parser: &vt100::Parser) -> bool {
    let screen = parser.screen();
    let (rows, cols) = screen.size();
    if rows == 0 {
        return false;
    }
    for r in rows.saturating_sub(6)..rows {
        let line = row_text(screen, r, cols);
        let trimmed = line.trim_start();
        if trimmed.starts_with("│ >") {
            return true;
        }
        if trimmed.starts_with("╭─") || trimmed.starts_with("╰─") {
            // A box edge near the bottom is a near-certain input frame.
            return true;
        }
    }
    false
}

/// Codex CLI uses a similar boxed-input pattern. Match on the same heuristics
/// plus a leading `▌` cursor glyph at the input area, which Codex renders.
fn codex_awaiting_input(parser: &vt100::Parser) -> bool {
    let screen = parser.screen();
    let (rows, cols) = screen.size();
    for r in rows.saturating_sub(6)..rows {
        let line = row_text(screen, r, cols);
        let trimmed = line.trim_start();
        if trimmed.starts_with('▌')
            || trimmed.starts_with("╭─")
            || trimmed.starts_with("╰─")
            || trimmed.starts_with("│ ")
        {
            return true;
        }
    }
    false
}

/// Gemini CLI uses a `>` prompt followed by content; cursor sits at the end of
/// a `> ` line.
fn gemini_awaiting_input(parser: &vt100::Parser) -> bool {
    let screen = parser.screen();
    let (rows, cols) = screen.size();
    for r in rows.saturating_sub(4)..rows {
        let line = row_text(screen, r, cols);
        let trimmed = line.trim_end();
        if trimmed == ">" || trimmed.ends_with("> ") || trimmed.starts_with("> ") {
            return true;
        }
    }
    false
}

/// Catch-all for `provider = "other"` agents: look for a prompt sigil at or
/// near the cursor.
fn generic_awaiting_input(parser: &vt100::Parser) -> bool {
    let screen = parser.screen();
    let (rows, cols) = screen.size();
    let (cur_row, _cur_col) = screen.cursor_position();
    let start = cur_row.saturating_sub(1);
    let end = cur_row.min(rows.saturating_sub(1));
    for r in start..=end {
        let line = row_text(screen, r, cols);
        let trimmed = line.trim_end();
        if trimmed.ends_with('>')
            || trimmed.ends_with('$')
            || trimmed.ends_with('❯')
            || trimmed.ends_with(':')
            || trimmed.ends_with('?')
        {
            return true;
        }
    }
    false
}

fn row_text(screen: &vt100::Screen, row: u16, cols: u16) -> String {
    let mut s = String::with_capacity(cols as usize);
    for c in 0..cols {
        if let Some(cell) = screen.cell(row, c) {
            let cs = cell.contents();
            if cs.is_empty() {
                s.push(' ');
            } else {
                s.push_str(&cs);
            }
        } else {
            s.push(' ');
        }
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::AgentConfig;
    use portable_pty::PtySize;
    use std::collections::BTreeMap;
    use std::io::{Read, Write};

    fn mock_agent(provider: Provider) -> Agent {
        let cfg = AgentConfig {
            id: "test".into(),
            name: None,
            provider,
            command: "echo".into(),
            args: vec![],
            cwd: None,
            env: BTreeMap::new(),
            manual: false,
        };
        let size = PtySize {
            rows: 24,
            cols: 80,
            pixel_width: 0,
            pixel_height: 0,
        };
        let now = Instant::now();
        Agent {
            rid: 1,
            name: "test".into(),
            provider,
            status: Status::Running,
            parser: vt100::Parser::new(24, 80, 1000),
            scroll_offset: 0,
            template: cfg,
            cwd_label: None,
            spawned_at: now - Duration::from_secs(10),
            last_output_at: now - Duration::from_secs(10),
            recent_bytes: 0,
            recent_window_start: now - Duration::from_secs(10),
            master: Box::new(MockMaster),
            writer: Box::new(Vec::new()),
            child: Box::new(MockChild),
            size,
        }
    }

    struct MockMaster;
    impl portable_pty::MasterPty for MockMaster {
        fn resize(&self, _size: PtySize) -> Result<(), anyhow::Error> {
            Ok(())
        }
        fn get_size(&self) -> Result<PtySize, anyhow::Error> {
            Ok(PtySize {
                rows: 24,
                cols: 80,
                pixel_width: 0,
                pixel_height: 0,
            })
        }
        fn try_clone_reader(&self) -> Result<Box<dyn Read + Send>, anyhow::Error> {
            Ok(Box::new(std::io::empty()))
        }
        fn take_writer(&self) -> Result<Box<dyn Write + Send>, anyhow::Error> {
            Ok(Box::new(std::io::sink()))
        }
        fn process_group_leader(&self) -> Option<i32> {
            None
        }
        fn as_raw_fd(&self) -> Option<std::os::unix::io::RawFd> {
            None
        }
    }

    #[derive(Debug)]
    struct MockChild;
    impl portable_pty::ChildKiller for MockChild {
        fn kill(&mut self) -> std::io::Result<()> {
            Ok(())
        }
        fn clone_killer(&self) -> Box<dyn portable_pty::ChildKiller + Send + Sync> {
            Box::new(MockKiller)
        }
    }
    impl portable_pty::Child for MockChild {
        fn try_wait(&mut self) -> std::io::Result<Option<portable_pty::ExitStatus>> {
            Ok(None)
        }
        fn wait(&mut self) -> std::io::Result<portable_pty::ExitStatus> {
            Ok(portable_pty::ExitStatus::with_exit_code(0))
        }
        fn process_id(&self) -> Option<u32> {
            None
        }
    }

    #[derive(Debug)]
    struct MockKiller;
    impl portable_pty::ChildKiller for MockKiller {
        fn kill(&mut self) -> std::io::Result<()> {
            Ok(())
        }
        fn clone_killer(&self) -> Box<dyn portable_pty::ChildKiller + Send + Sync> {
            Box::new(MockKiller)
        }
    }

    #[test]
    fn test_detect_idle() {
        let agent = mock_agent(Provider::Other);
        assert_eq!(detect(&agent), LiveState::Idle);
    }

    #[test]
    fn test_detect_working() {
        let mut agent = mock_agent(Provider::Other);
        agent.feed(b"some output");
        assert_eq!(detect(&agent), LiveState::Working);
    }

    #[test]
    fn test_detect_thinking() {
        let mut agent = mock_agent(Provider::Other);
        // Put a spinner in the bottom row
        let spinner = "⠋";
        let (rows, _cols) = agent.parser.screen().size();
        let bytes = format!("\x1b[{};1H{}", rows, spinner);
        agent.feed(bytes.as_bytes());
        assert_eq!(detect(&agent), LiveState::Thinking);
    }

    #[test]
    fn test_claude_waiting() {
        let mut agent = mock_agent(Provider::Claude);
        let (rows, _cols) = agent.parser.screen().size();
        let prompt = "\x1b[H\x1b[J"; // Clear
        agent.feed(prompt.as_bytes());

        // Move to bottom area and draw claude prompt
        let bytes = format!("\x1b[{};1H│ > hello", rows - 1);
        agent.feed(bytes.as_bytes());

        // Advance time to pass ACTIVE_WINDOW but stay within IDLE_DEADLINE for Waiting check
        agent.last_output_at = Instant::now() - Duration::from_secs(5);
        assert_eq!(detect(&agent), LiveState::Waiting);
    }

    #[test]
    fn test_gemini_waiting() {
        let mut agent = mock_agent(Provider::Gemini);
        let (rows, _cols) = agent.parser.screen().size();
        let bytes = format!("\x1b[{};1H> ", rows - 1);
        agent.feed(bytes.as_bytes());

        agent.last_output_at = Instant::now() - Duration::from_secs(5);
        assert_eq!(detect(&agent), LiveState::Waiting);
    }

    #[test]
    fn test_codex_waiting_box_corner() {
        let mut agent = mock_agent(Provider::Codex);
        let (rows, _cols) = agent.parser.screen().size();
        let bytes = format!("\x1b[{};1H╭─────────────╮", rows - 1);
        agent.feed(bytes.as_bytes());

        agent.last_output_at = Instant::now() - Duration::from_secs(5);
        assert_eq!(detect(&agent), LiveState::Waiting);
    }

    #[test]
    fn test_codex_waiting_cursor_glyph() {
        let mut agent = mock_agent(Provider::Codex);
        let (rows, _cols) = agent.parser.screen().size();
        let bytes = format!("\x1b[{};1H▌ type a message", rows - 1);
        agent.feed(bytes.as_bytes());

        agent.last_output_at = Instant::now() - Duration::from_secs(5);
        assert_eq!(detect(&agent), LiveState::Waiting);
    }

    #[test]
    fn test_aider_waiting_prompt() {
        let mut agent = mock_agent(Provider::Aider);
        let (rows, _cols) = agent.parser.screen().size();
        let bytes = format!("\x1b[{};1H> ", rows - 1);
        agent.feed(bytes.as_bytes());

        agent.last_output_at = Instant::now() - Duration::from_secs(5);
        assert_eq!(detect(&agent), LiveState::Waiting);
    }

    #[test]
    fn test_codex_not_waiting_when_active() {
        let mut agent = mock_agent(Provider::Codex);
        let (rows, _cols) = agent.parser.screen().size();
        let bytes = format!("\x1b[{};1H╭─────────────╮", rows - 1);
        agent.feed(bytes.as_bytes());

        // No backdating: recent output should keep us in Working, even though
        // the screen content matches the Codex input-prompt heuristic.
        assert_eq!(detect(&agent), LiveState::Working);
    }

    #[test]
    fn test_stuck() {
        let mut agent = mock_agent(Provider::Other);
        agent.last_output_at = Instant::now() - Duration::from_secs(60);
        assert_eq!(detect(&agent), LiveState::Stuck);
    }
}
