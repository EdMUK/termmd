//! Putting text on the clipboard from inside a terminal, including over ssh.
//!
//! OSC 52 is the sequence for it: the application hands the terminal some
//! base64 and the terminal, which is the one with a clipboard, sets it. It
//! works down an ssh connection for the same reason colour does -- the bytes
//! reach the terminal either way -- which is what makes it worth having in a
//! pager. The install line in a README is exactly the thing a reader wants to
//! take away, and selecting it with the mouse is the one operation a remote
//! terminal makes awkward.
//!
//! Inside tmux the sequence needs help. tmux only forwards an application's
//! OSC 52 when `set-clipboard` is `on`, and the default is `external`, which
//! means tmux uses the sequence for its own copies but will not pass one
//! through. Rather than asking people to change their configuration, we hand
//! the text to tmux and let it do the copying it was already willing to do.

use std::io::Write;
use std::process::{Command, Stdio};
use std::time::Duration;

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD as BASE64;

/// Ceiling on what we will try to copy. Terminals cap how long an escape
/// sequence may be, and quietly copying half a code block would be worse than
/// saying no to all of it.
const MAX_BYTES: usize = 64 * 1024;

/// How long tmux gets to take the text. Generous for 64KB down a pipe, and
/// short enough that a wedged server costs a pause rather than a pager.
const TMUX_TIMEOUT: Duration = Duration::from_secs(2);

/// What the caller must do to finish the copy.
#[derive(Debug, PartialEq, Eq)]
pub enum Copy {
    /// Write this to the terminal.
    Sequence(String),
    /// Already done, by a route that did not involve the terminal.
    Done,
}

/// Prepares `text` for the clipboard.
///
/// Fails only when there is nothing to copy or far too much of it: a terminal
/// that does not understand OSC 52 ignores it, and there is no reply to wait
/// for, so success here means "sent", not "arrived".
pub fn copy(text: &str) -> Result<Copy, &'static str> {
    if text.is_empty() {
        return Err("nothing to copy");
    }
    if text.len() > MAX_BYTES {
        return Err("too much to copy");
    }
    if in_tmux() && tmux_copy(text) {
        return Ok(Copy::Done);
    }
    Ok(Copy::Sequence(osc52(text)))
}

/// The OSC 52 sequence that sets the clipboard to `text`.
///
/// `c` is the clipboard selection proper, as opposed to the primary selection;
/// terminals that only implement one implement that one.
pub fn osc52(text: &str) -> String {
    format!("\x1b]52;c;{}\x07", BASE64.encode(text))
}

fn in_tmux() -> bool {
    std::env::var_os("TMUX").is_some()
}

/// Hands the text to tmux, which copies it to the client's clipboard itself.
///
/// `load-buffer -w -` reads from stdin, so a code block with a quote or a
/// newline in it needs no quoting and cannot outgrow an argument list.
fn tmux_copy(text: &str) -> bool {
    let Ok(mut child) = Command::new("tmux")
        .args(["load-buffer", "-w", "-"])
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
    else {
        return false;
    };
    if let Some(stdin) = child.stdin.as_mut() {
        if stdin.write_all(text.as_bytes()).is_err() {
            return false;
        }
    }
    // Closing stdin before waiting, or tmux waits for an end that never comes.
    drop(child.stdin.take());
    matches!(super::tmux::wait_within(&mut child, TMUX_TIMEOUT), Some(status) if status.success())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_sequence_carries_the_text_as_base64() {
        assert_eq!(osc52("hi"), "\x1b]52;c;aGk=\x07");
        // A newline and a quote are exactly what a code block contains, and
        // exactly what would break a sequence that was not encoded.
        let sequence = osc52("echo \"one\"\necho 'two'\n");
        assert!(sequence.starts_with("\x1b]52;c;"));
        assert!(sequence.ends_with('\x07'));
        let payload = sequence
            .trim_start_matches("\x1b]52;c;")
            .trim_end_matches('\x07');
        assert_eq!(
            String::from_utf8(BASE64.decode(payload).unwrap()).unwrap(),
            "echo \"one\"\necho 'two'\n"
        );
    }

    #[test]
    fn nothing_and_too_much_are_refused() {
        assert!(copy("").is_err());
        assert!(copy(&"x".repeat(MAX_BYTES + 1)).is_err());
    }

    #[test]
    fn a_reasonable_block_is_accepted() {
        // Outside tmux this is the escape route; inside it tmux may take it,
        // and either answer is a success.
        assert!(copy("cargo install termmd").is_ok());
    }
}
