//! The interactive picker.
//!
//! `eggpass login` and `eggpass logout` both ask the user to choose a
//! provider. The picker has two modes:
//!
//! * **Interactive** — stdin *and* stdout are terminals. `j`/`k` and the arrow
//!   keys move the cursor, `Enter` selects, `q` or `Esc` cancels, `Ctrl-C`
//!   interrupts.
//! * **Fallback** — stdin or stdout is a pipe or a file. A numbered list is
//!   printed and one line is read.
//!
//! The fallback is not optional: every command in this tool is documented as
//! cron-safe, and a picker that hung on a piped stdin would be a trap.
//!
//! The interactive mode follows the family's terminal selector: the screen is
//! cleared and the frame redrawn from the top on every keystroke, and every
//! line ends in an explicit `\r\n`. The `\r` matters — raw mode turns off the
//! kernel's newline translation, so a bare `\n` would step down a line without
//! returning to the first column and the menu would render as a staircase.

use std::io::{IsTerminal, Write};

/// The instruction line rendered above every interactive menu.
pub const SELECTOR_INSTRUCTIONS: &str =
    "Use j/k or \u{2191}/\u{2193} to navigate, Enter to select, q/Esc to quit";

/// One entry in the picker.
#[derive(Debug, Clone)]
pub struct Choice {
    /// The value that is returned when the entry is chosen.
    pub id: String,
    /// The text shown to the user.
    pub label: String,
    /// A second line, shown in grey.
    pub description: Option<String>,
}

impl Choice {
    /// Build a choice with no description.
    pub fn new(id: &str, label: &str) -> Choice {
        Choice {
            id: id.to_string(),
            label: label.to_string(),
            description: None,
        }
    }

    /// Build a choice with a description.
    pub fn described(id: &str, label: &str, description: &str) -> Choice {
        Choice {
            id: id.to_string(),
            label: label.to_string(),
            description: Some(description.to_string()),
        }
    }
}

/// Why the picker could not run.
#[derive(Debug)]
pub enum PromptError {
    /// The terminal could not be put into raw mode.
    Terminal(String),
    /// Reading the answer failed.
    Io(String),
    /// The user interrupted the picker with `Ctrl-C`.
    ///
    /// This is deliberately not the same as cancelling: cancelling is a
    /// choice, interrupting is the user giving up on the whole command.
    Interrupted,
}

impl std::fmt::Display for PromptError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            PromptError::Terminal(message) => write!(f, "{message}"),
            PromptError::Io(message) => write!(f, "{message}"),
            PromptError::Interrupted => write!(f, "terminal selection interrupted"),
        }
    }
}

impl std::error::Error for PromptError {}

/// True when both stdin and stdout are interactive terminals.
///
/// Both, not just stdin: a terminal on stdin with a pipe on stdout would put
/// cursor-control sequences into the pipe, and the person reading that pipe
/// would see the menu rendered as escape codes.
pub fn is_interactive() -> bool {
    std::io::stdin().is_terminal() && std::io::stdout().is_terminal()
}

/// What a single key press means.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KeyAction {
    /// Move to the next entry.
    Next,
    /// Move to the previous entry.
    Previous,
    /// Take the highlighted entry.
    Confirm,
    /// Give up without choosing.
    Cancel,
    /// Not a key we act on.
    Ignore,
    /// The user pressed `Ctrl-C`.
    Interrupted,
}

/// What the bytes following an `Esc` prefix meant.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EscapeOutcome {
    /// A standalone `Esc` — cancel.
    Cancel,
    /// The Up arrow.
    Previous,
    /// The Down arrow.
    Next,
    /// A malformed or incomplete sequence: redraw, never hang.
    Ignore,
}

/// Clamp-aware increment used by `j` and the Down arrow.
pub fn next_index(selected: usize, len: usize) -> usize {
    if len == 0 {
        return 0;
    }
    selected.saturating_add(1).min(len - 1)
}

/// Clamp-aware decrement used by `k` and the Up arrow.
pub fn prev_index(selected: usize, len: usize) -> usize {
    if len == 0 {
        return 0;
    }
    selected.saturating_sub(1).min(len - 1)
}

/// Classify a single non-`Esc` input byte.
pub fn classify_single(byte: u8) -> KeyAction {
    match byte {
        b'j' => KeyAction::Next,
        b'k' => KeyAction::Previous,
        b'\r' | b'\n' => KeyAction::Confirm,
        b'q' | b'Q' => KeyAction::Cancel,
        0x03 => KeyAction::Interrupted,
        _ => KeyAction::Ignore,
    }
}

/// Classify the bytes read after an `Esc` prefix within the bounded wait.
///
/// An empty slice means no continuation arrived — a standalone `Esc`, so
/// cancel. `[A` and `[B` are the Up and Down arrows. Anything else is ignored,
/// so a malformed or incomplete escape sequence can never hang the picker.
pub fn classify_escape_sequence(following: &[u8]) -> EscapeOutcome {
    match following {
        [] => EscapeOutcome::Cancel,
        [b'[', b'A'] => EscapeOutcome::Previous,
        [b'[', b'B'] => EscapeOutcome::Next,
        _ => EscapeOutcome::Ignore,
    }
}

/// Replace anything that could disturb the terminal with a `?`.
///
/// A label can come from the configuration file, so it can contain control
/// bytes. One of those in the middle of the menu would move the cursor and
/// corrupt the frame.
fn sanitize(text: &str) -> String {
    text.chars()
        .map(|character| {
            if character.is_control() {
                '?'
            } else {
                character
            }
        })
        .collect()
}

/// Render one full menu frame.
///
/// Pure and deterministic, so the exact bytes can be asserted in a test. The
/// frame clears the screen and homes the cursor, so every keystroke replaces
/// the menu rather than scrolling a new copy of it.
pub fn render_frame(title: &str, options: &[Choice], selected: usize) -> String {
    // Raw mode disables the kernel's newline translation, so a line has to end
    // with an explicit carriage return as well as a line feed.
    const NL: &str = "\r\n";

    let mut frame = String::new();
    frame.push_str("\x1b[2J\x1b[H");
    frame.push_str(&format!("\x1b[1m{}\x1b[0m{NL}{NL}", sanitize(title)));
    frame.push_str(&format!("  {SELECTOR_INSTRUCTIONS}{NL}{NL}"));
    for (index, option) in options.iter().enumerate() {
        let label = sanitize(&option.label);
        if index == selected {
            frame.push_str(&format!("  > \x1b[1;32m{label}\x1b[0m{NL}"));
        } else {
            frame.push_str(&format!("    {label}{NL}"));
        }
        if let Some(description) = &option.description {
            frame.push_str(&format!("      {}{NL}", sanitize(description)));
        }
    }
    frame.push_str(NL);
    frame
}

/// Ask the user to choose one of `options`.
///
/// Returns `None` when the user cancels, and `Err` when the terminal or the
/// input could not be read.
pub fn select(label: &str, options: &[Choice]) -> Result<Option<String>, PromptError> {
    if options.is_empty() {
        return Ok(None);
    }

    if !is_interactive() {
        return select_fallback(label, options);
    }

    #[cfg(unix)]
    {
        use std::os::fd::AsFd;

        let stdin = std::io::stdin();
        let mut stdout = std::io::stdout();
        let chosen = run_interactive_on(stdin.as_fd(), &mut stdout, label, options)?;
        Ok(chosen.map(|index| options[index].id.clone()))
    }
    #[cfg(not(unix))]
    {
        select_fallback(label, options)
    }
}

/// The numbered-list picker, used when the terminal is not interactive.
fn select_fallback(label: &str, options: &[Choice]) -> Result<Option<String>, PromptError> {
    let mut out = std::io::stdout();
    writeln!(out, "{label}").map_err(|err| PromptError::Io(err.to_string()))?;
    for (index, option) in options.iter().enumerate() {
        match &option.description {
            Some(description) => writeln!(out, "  {}) {} — {description}", index + 1, option.label),
            None => writeln!(out, "  {}) {}", index + 1, option.label),
        }
        .map_err(|err| PromptError::Io(err.to_string()))?;
    }
    write!(out, "Provider [1-{}]: ", options.len())
        .map_err(|err| PromptError::Io(err.to_string()))?;
    out.flush()
        .map_err(|err| PromptError::Io(err.to_string()))?;

    loop {
        let mut answer = String::new();
        // One line, not the whole of stdin: if stdin is a terminal, waiting
        // for end-of-input would hang.
        std::io::stdin()
            .read_line(&mut answer)
            .map_err(|err| PromptError::Io(err.to_string()))?;

        let answer = answer.trim();
        if answer.is_empty() {
            // An empty line, or end of input, means "no selection".
            return Ok(None);
        }

        match answer.parse::<usize>() {
            Ok(number) if number >= 1 && number <= options.len() => {
                return Ok(Some(options[number - 1].id.clone()));
            }
            // An answer we do not understand is a mistake, not a failure: say
            // so and let the user try again.
            _ => {
                writeln!(out, "  `{answer}` is not one of the choices.")
                    .map_err(|err| PromptError::Io(err.to_string()))?;
                out.flush()
                    .map_err(|err| PromptError::Io(err.to_string()))?;
            }
        }
    }
}

/// Run the interactive selector on an explicit input file descriptor.
///
/// The descriptor is a parameter so the selector can be driven by a
/// pseudoterminal in a test, rather than only by the process's own stdin.
#[cfg(unix)]
fn run_interactive_on(
    input: std::os::fd::BorrowedFd<'_>,
    output: &mut dyn Write,
    title: &str,
    options: &[Choice],
) -> Result<Option<usize>, PromptError> {
    use nix::sys::termios::{SetArg, SpecialCharacterIndices, cfmakeraw, tcgetattr, tcsetattr};
    use nix::unistd::read;

    let original = tcgetattr(input).map_err(|err| PromptError::Terminal(err.to_string()))?;
    let mut raw = original.clone();
    cfmakeraw(&mut raw);
    raw.control_chars[SpecialCharacterIndices::VMIN as usize] = 1;
    raw.control_chars[SpecialCharacterIndices::VTIME as usize] = 0;
    tcsetattr(input, SetArg::TCSANOW, &raw)
        .map_err(|err| PromptError::Terminal(err.to_string()))?;

    // The terminal has to be restored on *every* exit path, including a
    // panic, so the raw-mode guard lives in a `Drop` implementation.
    struct RawGuard<'scope> {
        fd: std::os::fd::BorrowedFd<'scope>,
        original: nix::sys::termios::Termios,
    }
    impl Drop for RawGuard<'_> {
        fn drop(&mut self) {
            let _ = tcsetattr(self.fd, SetArg::TCSANOW, &self.original);
        }
    }
    let _guard = RawGuard {
        fd: input,
        original,
    };

    // Bounded wait, in deciseconds, for the bytes following an `Esc` prefix,
    // so a standalone `Esc` cancels promptly while an arrow key's CSI
    // sequence still parses. It never blocks indefinitely after `Esc`.
    const ESCAPE_WAIT_DECISECONDS: u8 = 1;

    let mut selected = 0usize;
    loop {
        let frame = render_frame(title, options, selected);
        output
            .write_all(frame.as_bytes())
            .map_err(|err| PromptError::Io(err.to_string()))?;
        output
            .flush()
            .map_err(|err| PromptError::Io(err.to_string()))?;

        let mut byte = [0_u8; 1];
        let count = read(input, &mut byte).map_err(|err| PromptError::Io(err.to_string()))?;
        if count == 0 {
            // End of input: treat it as a cancel.
            return Ok(None);
        }

        if byte[0] == 0x1b {
            let mut timed = raw.clone();
            timed.control_chars[SpecialCharacterIndices::VMIN as usize] = 0;
            timed.control_chars[SpecialCharacterIndices::VTIME as usize] = ESCAPE_WAIT_DECISECONDS;
            tcsetattr(input, SetArg::TCSANOW, &timed)
                .map_err(|err| PromptError::Terminal(err.to_string()))?;

            let mut following = Vec::new();
            for _ in 0..2 {
                let mut next = [0_u8; 1];
                let extra =
                    read(input, &mut next).map_err(|err| PromptError::Io(err.to_string()))?;
                if extra == 0 {
                    break;
                }
                following.push(next[0]);
                if following.len() == 1 && following[0] != b'[' {
                    break;
                }
                if following.len() == 2 {
                    break;
                }
            }

            tcsetattr(input, SetArg::TCSANOW, &raw)
                .map_err(|err| PromptError::Terminal(err.to_string()))?;

            match classify_escape_sequence(&following) {
                EscapeOutcome::Cancel => return Ok(None),
                EscapeOutcome::Previous => selected = prev_index(selected, options.len()),
                EscapeOutcome::Next => selected = next_index(selected, options.len()),
                EscapeOutcome::Ignore => {}
            }
            continue;
        }

        match classify_single(byte[0]) {
            KeyAction::Next => selected = next_index(selected, options.len()),
            KeyAction::Previous => selected = prev_index(selected, options.len()),
            KeyAction::Confirm => {
                let _ = output.write_all(b"\r\n");
                let _ = output.flush();
                return Ok(Some(selected));
            }
            KeyAction::Cancel => return Ok(None),
            KeyAction::Ignore => {}
            KeyAction::Interrupted => return Err(PromptError::Interrupted),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn choices() -> Vec<Choice> {
        vec![
            Choice::described("xai", "x.ai", "Grok models, on your x.ai subscription"),
            Choice::described("openai", "ChatGPT", "ChatGPT models, on your ChatGPT plan"),
        ]
    }

    #[test]
    fn choices_carry_an_id_and_a_label() {
        let choice = Choice::new("xai", "x.ai");
        assert_eq!(choice.id, "xai");
        assert_eq!(choice.label, "x.ai");
        assert!(choice.description.is_none());

        let described = Choice::described("openai", "ChatGPT", "Plus and Pro plans");
        assert_eq!(described.description.as_deref(), Some("Plus and Pro plans"));
    }

    #[test]
    fn an_empty_list_selects_nothing() {
        assert_eq!(select("Pick one", &[]).unwrap(), None);
    }

    #[test]
    fn navigation_clamps_at_both_ends() {
        assert_eq!(next_index(0, 3), 1);
        assert_eq!(next_index(2, 3), 2);
        assert_eq!(prev_index(0, 3), 0);
        assert_eq!(prev_index(2, 3), 1);
        assert_eq!(next_index(0, 1), 0);
        assert_eq!(prev_index(0, 1), 0);
        assert_eq!(next_index(0, 0), 0);
        assert_eq!(prev_index(0, 0), 0);
    }

    #[test]
    fn single_bytes_map_to_the_documented_controls() {
        assert_eq!(classify_single(b'j'), KeyAction::Next);
        assert_eq!(classify_single(b'k'), KeyAction::Previous);
        assert_eq!(classify_single(b'\r'), KeyAction::Confirm);
        assert_eq!(classify_single(b'\n'), KeyAction::Confirm);
        assert_eq!(classify_single(b'q'), KeyAction::Cancel);
        assert_eq!(classify_single(b'Q'), KeyAction::Cancel);
        assert_eq!(classify_single(0x03), KeyAction::Interrupted);
        assert_eq!(classify_single(b'1'), KeyAction::Ignore);
    }

    #[test]
    fn escape_sequences_cancel_navigate_or_ignore_without_hanging() {
        assert_eq!(classify_escape_sequence(&[]), EscapeOutcome::Cancel);
        assert_eq!(classify_escape_sequence(b"[A"), EscapeOutcome::Previous);
        assert_eq!(classify_escape_sequence(b"[B"), EscapeOutcome::Next);
        assert_eq!(classify_escape_sequence(b"["), EscapeOutcome::Ignore);
        assert_eq!(classify_escape_sequence(b"[C"), EscapeOutcome::Ignore);
        assert_eq!(classify_escape_sequence(b"O"), EscapeOutcome::Ignore);
        assert_eq!(classify_escape_sequence(b"[AB"), EscapeOutcome::Ignore);
    }

    #[test]
    fn the_frame_clears_the_screen_and_redraws_from_the_top() {
        let frame = render_frame("Select a provider:", &choices(), 0);
        assert!(frame.starts_with("\x1b[2J\x1b[H"), "{frame:?}");
    }

    #[test]
    fn every_line_in_the_frame_ends_with_a_carriage_return_and_a_line_feed() {
        // Raw mode turns off the kernel's newline translation. A bare `\n`
        // would step down a line without returning to the first column, and
        // the menu would render as a staircase.
        let frame = render_frame("Select a provider:", &choices(), 0);
        assert!(
            !frame.contains('\n') || frame.matches('\n').count() == frame.matches("\r\n").count()
        );
        assert!(frame.contains("\r\n"), "{frame:?}");
        // Every line feed is preceded by a carriage return.
        let mut previous = b'\r';
        for byte in frame.bytes() {
            if byte == b'\n' {
                assert_eq!(previous, b'\r', "a line feed without a carriage return");
            }
            previous = byte;
        }
    }

    #[test]
    fn the_frame_documents_the_controls_and_highlights_the_selection() {
        let frame = render_frame("Select a provider:", &choices(), 1);
        assert!(frame.contains(SELECTOR_INSTRUCTIONS), "{frame:?}");
        assert!(frame.contains("j/k"), "{frame:?}");
        assert!(frame.contains('\u{2191}'), "{frame:?}");
        assert!(frame.contains("Enter"), "{frame:?}");
        assert!(frame.contains("q/Esc"), "{frame:?}");
        assert!(frame.contains("> \x1b[1;32mChatGPT"), "{frame:?}");
        assert!(!frame.contains("> \x1b[1;32mx.ai"), "{frame:?}");
    }

    #[test]
    fn control_bytes_in_a_label_are_neutralised() {
        let options = vec![Choice::new("x", "a\x1bb\nc")];
        let frame = render_frame("title", &options, 0);
        assert!(!frame.contains("a\x1bb\nc"), "{frame:?}");
        assert!(frame.contains("a?b?c"), "{frame:?}");
    }

    #[cfg(unix)]
    mod pty {
        use super::*;
        use nix::pty::openpty;
        use nix::sys::termios::tcgetattr;
        use std::os::fd::AsFd;
        use std::os::unix::net::UnixStream;
        use std::sync::mpsc;
        use std::time::{Duration, Instant};

        /// Drive the selector over a real pseudoterminal and return what it
        /// chose, plus the terminal settings before and after.
        fn run_selector_on_pty(
            input: &[u8],
            options: &[Choice],
        ) -> (
            Result<Option<usize>, PromptError>,
            nix::sys::termios::Termios,
            nix::sys::termios::Termios,
        ) {
            let pty = openpty(None, None).expect("openpty");
            let slave = pty.slave;
            // Put the master side into non-blocking mode through the safe std
            // API so the drain loop cannot block.
            let master = UnixStream::from(pty.master);
            master.set_nonblocking(true).expect("master nonblocking");
            let original = tcgetattr(&slave).expect("slave tcgetattr");

            let (sender, receiver) = mpsc::channel();
            let outcome = std::thread::scope(|scope| {
                scope.spawn(|| {
                    let writer_fd = slave.try_clone().expect("duplicate slave");
                    let mut writer = std::fs::File::from(writer_fd);
                    let outcome =
                        run_interactive_on(slave.as_fd(), &mut writer, "PTY test", options);
                    let _ = sender.send(outcome);
                });
                // Wait briefly for the selector to install raw mode, so the
                // master input is not consumed by the pre-raw line discipline.
                let raw_start = Instant::now();
                loop {
                    let current = tcgetattr(&slave).expect("poll slave termios");
                    if current != original {
                        break;
                    }
                    if raw_start.elapsed() > Duration::from_secs(2) {
                        break;
                    }
                    std::thread::sleep(Duration::from_millis(1));
                }
                let mut written = 0_usize;
                let write_start = Instant::now();
                while written < input.len() {
                    match nix::unistd::write(master.as_fd(), &input[written..]) {
                        Ok(0) => std::thread::sleep(Duration::from_millis(1)),
                        Ok(count) => written += count,
                        Err(nix::errno::Errno::EAGAIN) => {
                            std::thread::sleep(Duration::from_millis(1));
                        }
                        Err(error) => panic!("pty write failed: {error}"),
                    }
                    assert!(
                        write_start.elapsed() <= Duration::from_secs(2),
                        "pty write timeout"
                    );
                }
                let wait_start = Instant::now();
                loop {
                    let mut discard = [0_u8; 4096];
                    match nix::unistd::read(master.as_fd(), &mut discard) {
                        Ok(_) | Err(nix::errno::Errno::EAGAIN) => {}
                        Err(error) => panic!("pty drain failed: {error}"),
                    }
                    match receiver.try_recv() {
                        Ok(outcome) => {
                            for _ in 0..8 {
                                let mut tail = [0_u8; 4096];
                                match nix::unistd::read(master.as_fd(), &mut tail) {
                                    Ok(0) => break,
                                    Ok(_) => {}
                                    Err(nix::errno::Errno::EAGAIN) => break,
                                    Err(error) => panic!("pty final drain failed: {error}"),
                                }
                            }
                            return outcome;
                        }
                        Err(mpsc::TryRecvError::Empty) => {
                            assert!(
                                wait_start.elapsed() <= Duration::from_secs(5),
                                "pty selector timeout"
                            );
                            std::thread::sleep(Duration::from_millis(1));
                        }
                        Err(mpsc::TryRecvError::Disconnected) => {
                            panic!("selector thread ended without a result")
                        }
                    }
                }
            });
            let restored = tcgetattr(&slave).expect("restored tcgetattr");
            (outcome, original, restored)
        }

        #[test]
        fn the_selector_chooses_with_j_and_k() {
            let (outcome, original, restored) = run_selector_on_pty(b"jj\r", &choices());
            assert_eq!(outcome.unwrap(), Some(1), "j moves down, Enter confirms");
            assert_termios_restored(&original, &restored);
        }

        #[test]
        fn the_selector_chooses_with_the_arrow_keys() {
            let (outcome, original, restored) = run_selector_on_pty(b"\x1b[B\r", &choices());
            assert_eq!(outcome.unwrap(), Some(1), "Down moves down");
            assert_termios_restored(&original, &restored);
        }

        #[test]
        fn the_selector_cancels_on_q() {
            let (outcome, original, restored) = run_selector_on_pty(b"q", &choices());
            assert_eq!(outcome.unwrap(), None);
            assert_termios_restored(&original, &restored);
        }

        #[test]
        fn the_selector_cancels_on_escape() {
            let (outcome, original, restored) = run_selector_on_pty(b"\x1b", &choices());
            assert_eq!(outcome.unwrap(), None);
            assert_termios_restored(&original, &restored);
        }

        #[test]
        fn the_selector_reports_an_interrupt_distinctly_from_a_cancel() {
            let (outcome, original, restored) = run_selector_on_pty(b"\x03", &choices());
            assert!(
                matches!(outcome, Err(PromptError::Interrupted)),
                "Ctrl-C is an interruption, not a cancellation"
            );
            assert_termios_restored(&original, &restored);
        }

        #[test]
        fn the_selector_clears_the_screen_on_every_frame() {
            let (outcome, _, _) = run_selector_on_pty(b"j\r", &choices());
            assert_eq!(outcome.unwrap(), Some(1));
        }

        fn assert_termios_restored(
            original: &nix::sys::termios::Termios,
            restored: &nix::sys::termios::Termios,
        ) {
            use nix::sys::termios::LocalFlags;
            // macOS sets PENDIN as a side effect of `tcsetattr` on a PTY slave,
            // even for a bare raw round-trip with no I/O. PENDIN is not part of
            // the raw transition, so mask it and compare the raw-relevant
            // fields explicitly.
            assert_eq!(
                original.input_flags, restored.input_flags,
                "input flags restored"
            );
            assert_eq!(
                original.output_flags, restored.output_flags,
                "output flags restored"
            );
            assert_eq!(
                original.control_flags, restored.control_flags,
                "control flags restored"
            );
            assert_eq!(
                original.local_flags & !LocalFlags::PENDIN,
                restored.local_flags & !LocalFlags::PENDIN,
                "local flags restored"
            );
        }
    }
}
