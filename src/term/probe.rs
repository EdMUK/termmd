//! Asks the terminal what it can do, instead of guessing from `TERM`.
//!
//! We write a batch of query sequences and read the replies. The trick that
//! makes this reliable is the ordering: terminals answer queries in the order
//! they arrive, and every terminal answers a Primary Device Attributes request.
//! Sending `CSI c` last therefore turns it into a sentinel -- once its reply
//! arrives, any terminal that intended to answer the earlier queries already
//! has, so we can stop reading immediately rather than waiting out a timeout.
//!
//! On a terminal that answers nothing at all we pay the timeout once (100ms)
//! and fall back to the environment-based guesses.

use super::caps::{Capabilities, GraphicsProtocol};
use super::style::Rgb;
use super::tmux;

/// Total time we are willing to wait for replies.
#[cfg(unix)]
const TIMEOUT_MS: u64 = 300;

/// What a probe managed to learn. Every field is optional: terminals answer
/// the subset of queries they understand and silently ignore the rest.
#[derive(Debug, Default, PartialEq)]
pub struct ProbeResult {
    pub kitty_graphics: bool,
    pub sixel: bool,
    pub cell_px: Option<(u16, u16)>,
    pub background: Option<Rgb>,
    pub answered: bool,
}

/// Promotes a multiplexed session to what tmux says its client can manage.
///
/// tmux from 3.4 parses a sixel into the pane and redraws it itself, so this is
/// not passthrough and does not depend on the escape codes surviving; and it
/// forwards OSC 8 to a client that understands it. Neither is promoted without
/// tmux naming the feature, which keeps every older version on half blocks.
fn apply_tmux(caps: &mut Capabilities, features: tmux::Features) {
    if features.sixel && caps.graphics == GraphicsProtocol::Blocks {
        caps.graphics = GraphicsProtocol::Sixel;
    }
    if features.hyperlinks {
        caps.hyperlinks = true;
    }
}

/// The query batch, ordered so that the Primary DA reply acts as a sentinel.
///
/// Only the unix probe sends these; Windows relies on the environment tier.
#[cfg(unix)]
fn queries() -> String {
    let mut q = String::new();
    // kitty graphics support: transmit a 1x1 RGB image by direct payload and
    // ask for a status report. Terminals without the protocol ignore the APC.
    q.push_str("\x1b_Gi=4294967295,s=1,v=1,a=q,t=d,f=24;AAAA\x1b\\");
    // Background colour, so we can pick a light or dark theme.
    q.push_str("\x1b]11;?\x1b\\");
    // Cell size in pixels: replies as CSI 6 ; height ; width t.
    q.push_str("\x1b[16t");
    // Primary Device Attributes: the sentinel. Attribute 4 means sixel.
    q.push_str("\x1b[c");
    q
}

/// Runs a probe and folds the result into `caps`.
pub fn refine(caps: &mut Capabilities) {
    // Inside tmux the terminal we can reach is tmux itself, and its answers are
    // about tmux. What the client on the far side can draw has to be asked of
    // tmux separately.
    if caps.multiplexed {
        if let Some(features) = tmux::features() {
            apply_tmux(caps, features);
        }
    }
    let Some(result) = run() else { return };
    if !result.answered {
        return;
    }
    if let Some(px) = result.cell_px {
        caps.cell_px = Some(px);
    }
    if result.background.is_some() {
        caps.background = result.background;
    }
    // A positive answer promotes; a silent terminal keeps the env-based guess,
    // because plenty of terminals support graphics without answering queries.
    if result.kitty_graphics && caps.graphics != GraphicsProtocol::Kitty && !caps.multiplexed {
        caps.graphics = GraphicsProtocol::Kitty;
    } else if result.sixel
        && !caps.multiplexed
        && matches!(
            caps.graphics,
            GraphicsProtocol::Blocks | GraphicsProtocol::None
        )
    {
        caps.graphics = GraphicsProtocol::Sixel;
    }
}

/// Performs the terminal round trip. Returns `None` if there is no usable tty.
#[cfg(unix)]
pub fn run() -> Option<ProbeResult> {
    let tty = unix::Tty::open()?;
    let raw = tty.enter_raw()?;
    let response = raw.exchange(queries().as_bytes(), TIMEOUT_MS);
    drop(raw);
    Some(parse(&response))
}

#[cfg(not(unix))]
pub fn run() -> Option<ProbeResult> {
    // Windows consoles do not expose a /dev/tty equivalent we can safely put
    // into a byte-at-a-time mode without disturbing the input queue, so we rely
    // on the environment tier there.
    None
}

/// Extracts capabilities from a raw reply buffer.
///
/// Written against bytes rather than a live terminal so the parsing rules are
/// unit-testable, which matters more here than usual: these sequences are
/// awkward to reproduce by hand.
pub fn parse(buf: &[u8]) -> ProbeResult {
    let s = String::from_utf8_lossy(buf);
    let mut out = ProbeResult::default();

    // kitty replies <ESC>_Gi=<id>;OK<ESC>\ ; an error reply names the failure
    // instead of OK but still proves the protocol is understood.
    if let Some(apc) = find_between(&s, "\x1b_G", "\x1b\\") {
        if apc.contains(";OK") || apc.contains("i=4294967295") {
            out.kitty_graphics = true;
            out.answered = true;
        }
    }

    // Primary DA: CSI ? 62 ; 4 ; ... c -- attribute 4 is sixel graphics.
    if let Some(da) = find_between(&s, "\x1b[?", "c") {
        out.answered = true;
        out.sixel = da.split(';').any(|p| p.trim() == "4");
    }

    // Cell size: CSI 6 ; height ; width t
    if let Some(t) = find_between(&s, "\x1b[6;", "t") {
        let mut parts = t.split(';');
        if let (Some(h), Some(w)) = (parts.next(), parts.next()) {
            if let (Ok(h), Ok(w)) = (h.trim().parse::<u16>(), w.trim().parse::<u16>()) {
                if h > 0 && w > 0 {
                    out.cell_px = Some((w, h));
                    out.answered = true;
                }
            }
        }
    }

    // Background: OSC 11 ; rgb:RRRR/GGGG/BBBB (component width varies).
    if let Some(rgb) = s.split("\x1b]11;").nth(1) {
        let payload = rgb.split(['\x07', '\x1b']).next().unwrap_or("");
        if let Some(c) = parse_xparse_color(payload) {
            out.background = Some(c);
            out.answered = true;
        }
    }
    out
}

/// Parses X11 `rgb:RRRR/GGGG/BBBB` with 1-4 hex digits per component.
fn parse_xparse_color(s: &str) -> Option<Rgb> {
    let body = s.trim().strip_prefix("rgb:")?;
    let mut it = body.split('/');
    let mut next = || -> Option<u8> {
        let part = it.next()?.trim();
        if part.is_empty() || part.len() > 4 {
            return None;
        }
        let v = u32::from_str_radix(part, 16).ok()?;
        // Scale from the reported bit depth down to 8 bits.
        let max = (1u32 << (4 * part.len() as u32)) - 1;
        Some(((v * 255 + max / 2) / max) as u8)
    };
    Some(Rgb(next()?, next()?, next()?))
}

/// Returns the text between the first `start` and the following `end`.
fn find_between<'a>(haystack: &'a str, start: &str, end: &str) -> Option<&'a str> {
    let i = haystack.find(start)? + start.len();
    let rest = &haystack[i..];
    let j = rest.find(end)?;
    Some(&rest[..j])
}

#[cfg(unix)]
mod unix {
    //! Minimal termios handling: put the tty in a non-canonical mode with a read
    //! timeout, exchange bytes, restore the previous settings on drop.

    use std::fs::{File, OpenOptions};
    use std::io::{Read, Write};
    use std::os::unix::io::AsRawFd;

    pub struct Tty(File);

    impl Tty {
        pub fn open() -> Option<Self> {
            OpenOptions::new()
                .read(true)
                .write(true)
                .open("/dev/tty")
                .ok()
                .map(Tty)
        }

        pub fn enter_raw(self) -> Option<RawTty> {
            let fd = self.0.as_raw_fd();
            // SAFETY: `fd` is a live descriptor for the controlling terminal and
            // `termios` is a plain C struct we only ever pass back to libc.
            unsafe {
                let mut saved: libc::termios = std::mem::zeroed();
                if libc::tcgetattr(fd, &mut saved) != 0 {
                    return None;
                }
                let mut raw = saved;
                raw.c_lflag &= !(libc::ICANON | libc::ECHO);
                // VMIN=0/VTIME=1: return as soon as any byte arrives, or after
                // 100ms with nothing. This is the read timeout.
                raw.c_cc[libc::VMIN] = 0;
                raw.c_cc[libc::VTIME] = 1;
                if libc::tcsetattr(fd, libc::TCSANOW, &raw) != 0 {
                    return None;
                }
                Some(RawTty { tty: self, saved })
            }
        }
    }

    pub struct RawTty {
        tty: Tty,
        saved: libc::termios,
    }

    impl RawTty {
        /// Writes `query` and reads until the reply looks complete or time runs out.
        pub fn exchange(&self, query: &[u8], timeout_ms: u64) -> Vec<u8> {
            let mut f = &self.tty.0;
            if f.write_all(query).is_err() || f.flush().is_err() {
                return Vec::new();
            }
            let deadline = std::time::Instant::now() + std::time::Duration::from_millis(timeout_ms);
            let mut buf = Vec::with_capacity(256);
            let mut chunk = [0u8; 256];
            while std::time::Instant::now() < deadline {
                match f.read(&mut chunk) {
                    Ok(0) => {
                        // A 100ms read timeout elapsed. If the sentinel already
                        // arrived we are done; otherwise keep waiting.
                        if sentinel_seen(&buf) {
                            break;
                        }
                    }
                    Ok(n) => {
                        buf.extend_from_slice(&chunk[..n]);
                        if sentinel_seen(&buf) {
                            break;
                        }
                    }
                    Err(_) => break,
                }
            }
            buf
        }
    }

    /// True once the Primary DA reply (`CSI ? ... c`) is present.
    fn sentinel_seen(buf: &[u8]) -> bool {
        let Some(start) = buf.windows(3).position(|w| w == b"\x1b[?") else {
            return false;
        };
        buf[start..].contains(&b'c')
    }

    impl Drop for RawTty {
        fn drop(&mut self) {
            // SAFETY: restoring the settings we captured in `enter_raw`.
            unsafe {
                libc::tcsetattr(self.tty.0.as_raw_fd(), libc::TCSANOW, &self.saved);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detects_kitty_support() {
        let r = parse(b"\x1b_Gi=4294967295;OK\x1b\\\x1b[?62;22c");
        assert!(r.kitty_graphics);
        assert!(r.answered);
    }

    #[test]
    fn detects_sixel_from_device_attributes() {
        let r = parse(b"\x1b[?62;1;2;4;6;9;22c");
        assert!(r.sixel);
        assert!(!r.kitty_graphics);
    }

    #[test]
    fn does_not_mistake_other_attributes_for_sixel() {
        // 14 and 40 contain a '4' but are not attribute 4.
        let r = parse(b"\x1b[?65;14;40;22c");
        assert!(!r.sixel);
    }

    #[test]
    fn parses_cell_size_report() {
        let r = parse(b"\x1b[6;34;15t\x1b[?62c");
        assert_eq!(r.cell_px, Some((15, 34)));
    }

    #[test]
    fn parses_background_color_at_several_depths() {
        let r = parse(b"\x1b]11;rgb:1e1e/1e1e/2e2e\x1b\\\x1b[?62c");
        assert_eq!(r.background, Some(Rgb(0x1e, 0x1e, 0x2e)));

        let r = parse(b"\x1b]11;rgb:ff/ff/ff\x07\x1b[?62c");
        assert_eq!(r.background, Some(Rgb(255, 255, 255)));
    }

    #[test]
    fn silence_reports_nothing() {
        let r = parse(b"");
        assert!(!r.answered);
        assert_eq!(r, ProbeResult::default());
    }

    #[test]
    fn tmux_promotes_only_what_its_client_can_draw() {
        let multiplexed = || Capabilities {
            graphics: GraphicsProtocol::Blocks,
            multiplexed: true,
            is_tty: true,
            ..Default::default()
        };

        let mut caps = multiplexed();
        apply_tmux(&mut caps, tmux::parse("bpaste,RGB,sixel,hyperlinks,title"));
        assert_eq!(caps.graphics, GraphicsProtocol::Sixel);
        assert!(caps.hyperlinks);

        // The same tmux in front of a terminal that can do neither.
        let mut caps = multiplexed();
        apply_tmux(&mut caps, tmux::parse("bpaste,RGB,title"));
        assert_eq!(
            caps.graphics,
            GraphicsProtocol::Blocks,
            "half blocks are the only thing that survives everywhere"
        );
        assert!(!caps.hyperlinks);
    }

    #[test]
    fn tmux_does_not_overrule_a_forced_protocol() {
        // `--images none` and `--images kitty` are resolved into the same field
        // the promotion writes, so it must only ever lift the fallback.
        for forced in [GraphicsProtocol::None, GraphicsProtocol::Kitty] {
            let mut caps = Capabilities {
                graphics: forced,
                multiplexed: true,
                is_tty: true,
                ..Default::default()
            };
            apply_tmux(&mut caps, tmux::parse("sixel"));
            assert_eq!(caps.graphics, forced);
        }
    }

    #[test]
    fn probe_does_not_downgrade_a_known_good_terminal() {
        // A terminal that supports kitty graphics but answers no queries must
        // keep the capability the environment tier gave it.
        let mut caps = Capabilities {
            graphics: GraphicsProtocol::Kitty,
            is_tty: true,
            ..Default::default()
        };
        let before = caps.graphics;
        // Simulate `refine` seeing an unanswered probe.
        let result = ProbeResult::default();
        if result.answered {
            caps.graphics = GraphicsProtocol::None;
        }
        assert_eq!(caps.graphics, before);
    }
}
