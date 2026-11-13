//! The interactive picker.
//!
//! `eggpass login` and `eggpass logout` both ask the user to choose a
//! provider. The picker has two modes:
//!
//! * **Interactive** — stdin is a terminal. Arrow keys and `j`/`k` move the
//!   cursor, `Enter` selects, `Esc` and `Ctrl-C` cancel.
//! * **Fallback** — stdin is a pipe or a file. A numbered list is printed and
//!   one line is read.
//!
//! The fallback is not optional: every command in this tool is documented as
//! cron-safe, and a picker that hung on a piped stdin would be a trap.

use std::io::{IsTerminal, Read, Write};

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
}

impl std::fmt::Display for PromptError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            PromptError::Terminal(message) => write!(f, "{message}"),
            PromptError::Io(message) => write!(f, "{message}"),
        }
    }
}

impl std::error::Error for PromptError {}

/// Ask the user to choose one of `options`.
///
/// Returns `None` when the user cancels, and `Err` when the terminal or the
/// input could not be read.
pub fn select(label: &str, options: &[Choice]) -> Result<Option<String>, PromptError> {
    if options.is_empty() {
        return Ok(None);
    }

    if std::io::stdin().is_terminal() {
        select_interactive(label, options)
    } else {
        select_fallback(label, options)
    }
}

/// The raw-mode picker.
fn select_interactive(label: &str, options: &[Choice]) -> Result<Option<String>, PromptError> {
    let mut terminal = Terminal::new()?;
    let mut cursor = 0usize;

    terminal.draw(label, options, cursor)?;

    loop {
        let event = crossterm::event::read()
            .map_err(|err| PromptError::Terminal(format!("could not read the keyboard: {err}")))?;

        let crossterm::event::Event::Key(key) = event else {
            continue;
        };

        use crossterm::event::{KeyCode, KeyEventKind, KeyModifiers};

        // Windows reports key presses twice; only act on the press.
        if key.kind == KeyEventKind::Release {
            continue;
        }

        match key.code {
            KeyCode::Up | KeyCode::Char('k') => {
                cursor = cursor.checked_sub(1).unwrap_or(options.len() - 1);
            }
            KeyCode::Down | KeyCode::Char('j') => {
                cursor = (cursor + 1) % options.len();
            }
            KeyCode::Enter => {
                terminal.finish()?;
                return Ok(Some(options[cursor].id.clone()));
            }
            KeyCode::Esc => {
                terminal.finish()?;
                return Ok(None);
            }
            KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                terminal.finish()?;
                return Ok(None);
            }
            _ => continue,
        }

        terminal.draw(label, options, cursor)?;
    }
}

/// The numbered-list picker, used when stdin is not a terminal.
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

    let mut answer = String::new();
    std::io::stdin()
        .read_to_string(&mut answer)
        .map_err(|err| PromptError::Io(err.to_string()))?;

    // An empty line, or end of input, means "no selection".
    let answer = answer.trim();
    if answer.is_empty() {
        return Ok(None);
    }

    let choice = match answer.parse::<usize>() {
        Ok(number) if number >= 1 && number <= options.len() => options[number - 1].clone(),
        // An answer we do not understand is a mistake, not a failure: say so
        // and let the user try again.
        _ => {
            writeln!(out, "  `{answer}` is not one of the choices.")
                .map_err(|err| PromptError::Io(err.to_string()))?;
            return select_fallback(label, options);
        }
    };

    Ok(Some(choice.id))
}

/// Owns the terminal while the picker is on screen.
///
/// The terminal has to be restored on *every* exit path, including a panic,
/// so the raw-mode guard lives in a `Drop` implementation.
struct Terminal {
    raw: bool,
    lines: usize,
}

impl Terminal {
    /// Put the terminal into raw mode.
    fn new() -> Result<Terminal, PromptError> {
        crossterm::terminal::enable_raw_mode()
            .map_err(|err| PromptError::Terminal(format!("could not enter raw mode: {err}")))?;
        let terminal = Terminal {
            raw: true,
            lines: 0,
        };
        let _ = crossterm::execute!(
            std::io::stdout(),
            crossterm::cursor::Hide,
            crossterm::terminal::Clear(crossterm::terminal::ClearType::All)
        );
        Ok(terminal)
    }

    /// Draw the menu, replacing the previous one.
    fn draw(&mut self, label: &str, options: &[Choice], cursor: usize) -> Result<(), PromptError> {
        let mut out = std::io::stdout();

        if self.lines > 0 {
            // Move back over the previous menu.
            write!(out, "\x1b[{}A", self.lines).map_err(|err| PromptError::Io(err.to_string()))?;
            // Clear from the cursor down.
            write!(out, "\x1b[J").map_err(|err| PromptError::Io(err.to_string()))?;
        }

        let mut lines = 0usize;
        writeln!(out, "{label}").map_err(|err| PromptError::Io(err.to_string()))?;
        lines += 1;
        writeln!(out).map_err(|err| PromptError::Io(err.to_string()))?;
        lines += 1;

        for (index, option) in options.iter().enumerate() {
            let marker = if index == cursor { "❯" } else { " " };
            writeln!(out, "  {marker} {}", option.label)
                .map_err(|err| PromptError::Io(err.to_string()))?;
            lines += 1;
            if let Some(description) = &option.description {
                writeln!(out, "      {description}")
                    .map_err(|err| PromptError::Io(err.to_string()))?;
                lines += 1;
            }
        }

        writeln!(out).map_err(|err| PromptError::Io(err.to_string()))?;
        lines += 1;
        writeln!(
            out,
            "  ↑/↓ or j/k to move · Enter to select · Esc to cancel"
        )
        .map_err(|err| PromptError::Io(err.to_string()))?;
        lines += 1;

        out.flush()
            .map_err(|err| PromptError::Io(err.to_string()))?;
        self.lines = lines;
        Ok(())
    }

    /// Leave raw mode and show the cursor again.
    fn finish(&mut self) -> Result<(), PromptError> {
        let _ = crossterm::execute!(std::io::stdout(), crossterm::cursor::Show);
        if self.raw {
            let _ = crossterm::terminal::disable_raw_mode();
            self.raw = false;
        }
        Ok(())
    }
}

impl Drop for Terminal {
    fn drop(&mut self) {
        if self.raw {
            let _ = crossterm::execute!(std::io::stdout(), crossterm::cursor::Show);
            let _ = crossterm::terminal::disable_raw_mode();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
}
