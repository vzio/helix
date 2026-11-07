//! Finds the OSC 133 shell integration markers in the output of the shell, before it goes to
//! the emulator (which ignores them):
//!
//! - `ESC ] 133 ; A ST`: the prompt starts
//! - `ESC ] 133 ; B ST`: the prompt ends, the user types the command
//! - `ESC ] 133 ; C ST`: the command runs
//! - `ESC ] 133 ; D [; exit code] ST`: the command finished
//!
//! where ST is `BEL` or `ESC \`. A marker may be split between two reads.

/// A shell integration marker.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Marker {
    /// `A`: the prompt starts.
    PromptStart,
    /// `B`: the prompt ends, the user types the command.
    CommandStart,
    /// `C`: the command runs.
    CommandExecuted,
    /// `D`: the command finished, with its exit code when the shell gave one.
    CommandFinished(Option<i32>),
}

/// The longest OSC that is kept to look at; longer ones (clipboard, images...) are skipped.
const MAX_OSC_LEN: usize = 128;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
enum State {
    #[default]
    Ground,
    /// Saw `ESC` outside of an OSC.
    Escape,
    /// Inside `ESC ]` ...
    Osc,
    /// Saw `ESC` inside an OSC: `\` ends it.
    OscEscape,
}

/// Incremental scanner for the markers: feed it the output of the shell chunk by chunk.
#[derive(Debug, Default)]
pub struct Scanner {
    state: State,
    buf: Vec<u8>,
    /// The OSC being read is too long to be a marker.
    overflow: bool,
}

impl Scanner {
    pub fn new() -> Self {
        Self::default()
    }

    /// Scans a chunk of output, returning the markers that ended in it, in order. The bytes
    /// themselves are not changed: the emulator still gets all of them.
    pub fn feed(&mut self, bytes: &[u8]) -> Vec<Marker> {
        let mut markers = Vec::new();
        for &byte in bytes {
            match self.state {
                State::Ground => {
                    if byte == 0x1b {
                        self.state = State::Escape;
                    }
                }
                State::Escape => match byte {
                    b']' => self.start_osc(),
                    0x1b => {}
                    _ => self.state = State::Ground,
                },
                State::Osc => match byte {
                    0x07 => self.end_osc(&mut markers),
                    0x1b => self.state = State::OscEscape,
                    // CAN and SUB cancel the sequence.
                    0x18 | 0x1a => self.state = State::Ground,
                    _ => {
                        if self.buf.len() < MAX_OSC_LEN {
                            self.buf.push(byte);
                        } else {
                            self.overflow = true;
                        }
                    }
                },
                State::OscEscape => match byte {
                    b'\\' => self.end_osc(&mut markers),
                    // `ESC ]` starts a new OSC, anything else is a new escape sequence.
                    b']' => self.start_osc(),
                    0x1b => {}
                    _ => self.state = State::Ground,
                },
            }
        }
        markers
    }

    fn start_osc(&mut self) {
        self.state = State::Osc;
        self.buf.clear();
        self.overflow = false;
    }

    fn end_osc(&mut self, markers: &mut Vec<Marker>) {
        self.state = State::Ground;
        if !self.overflow {
            if let Some(marker) = parse(&self.buf) {
                markers.push(marker);
            }
        }
        self.buf.clear();
    }
}

/// Parses the body of an OSC: `133;A`, `133;D;1`, `133;D;1;key=value`...
fn parse(body: &[u8]) -> Option<Marker> {
    let rest = body.strip_prefix(b"133;")?;
    let mut params = rest.split(|&b| b == b';');
    let code = params.next()?;
    match code {
        b"A" => Some(Marker::PromptStart),
        b"B" => Some(Marker::CommandStart),
        b"C" => Some(Marker::CommandExecuted),
        b"D" => {
            let exit_code = params
                .next()
                .and_then(|p| std::str::from_utf8(p).ok())
                .and_then(|p| p.parse::<i32>().ok());
            Some(Marker::CommandFinished(exit_code))
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scan(bytes: &[u8]) -> Vec<Marker> {
        Scanner::new().feed(bytes)
    }

    /// Feeds `bytes` in two chunks split at every position, and byte by byte.
    fn scan_everywhere(bytes: &[u8]) -> Vec<Vec<Marker>> {
        let mut all = Vec::new();
        for at in 0..=bytes.len() {
            let mut scanner = Scanner::new();
            let mut markers = scanner.feed(&bytes[..at]);
            markers.extend(scanner.feed(&bytes[at..]));
            all.push(markers);
        }
        let mut scanner = Scanner::new();
        all.push(bytes.iter().flat_map(|b| scanner.feed(&[*b])).collect());
        all
    }

    #[test]
    fn each_marker() {
        assert_eq!(scan(b"\x1b]133;A\x07"), [Marker::PromptStart]);
        assert_eq!(scan(b"\x1b]133;B\x07"), [Marker::CommandStart]);
        assert_eq!(scan(b"\x1b]133;C\x07"), [Marker::CommandExecuted]);
        assert_eq!(
            scan(b"\x1b]133;D;3\x07"),
            [Marker::CommandFinished(Some(3))]
        );
    }

    #[test]
    fn string_terminator() {
        assert_eq!(scan(b"\x1b]133;A\x1b\\"), [Marker::PromptStart]);
        assert_eq!(
            scan(b"\x1b]133;D;7\x1b\\"),
            [Marker::CommandFinished(Some(7))]
        );
    }

    #[test]
    fn finished_with_and_without_code() {
        assert_eq!(scan(b"\x1b]133;D\x07"), [Marker::CommandFinished(None)]);
        assert_eq!(scan(b"\x1b]133;D;\x07"), [Marker::CommandFinished(None)]);
        assert_eq!(
            scan(b"\x1b]133;D;0\x07"),
            [Marker::CommandFinished(Some(0))]
        );
        assert_eq!(
            scan(b"\x1b]133;D;-1\x07"),
            [Marker::CommandFinished(Some(-1))]
        );
        assert_eq!(
            scan(b"\x1b]133;D;130\x07"),
            [Marker::CommandFinished(Some(130))]
        );
    }

    #[test]
    fn extra_params() {
        assert_eq!(
            scan(b"\x1b]133;D;2;aid=12\x07"),
            [Marker::CommandFinished(Some(2))]
        );
        assert_eq!(
            scan(b"\x1b]133;D;err=1\x07"),
            [Marker::CommandFinished(None)]
        );
        assert_eq!(scan(b"\x1b]133;A;aid=1;cl=m\x07"), [Marker::PromptStart]);
        assert_eq!(
            scan(b"\x1b]133;C;cmdline=ls\x1b\\"),
            [Marker::CommandExecuted]
        );
    }

    #[test]
    fn other_sequences_are_not_markers() {
        assert_eq!(scan(b"\x1b]0;title\x07"), []);
        assert_eq!(scan(b"\x1b]1330;A\x07"), []);
        assert_eq!(scan(b"\x1b]133;X\x07"), []);
        assert_eq!(scan(b"\x1b]133;AB\x07"), []);
        assert_eq!(scan(b"\x1b[31m133;A\x07"), []);
        assert_eq!(scan(b"133;A"), []);
        // Not terminated yet.
        assert_eq!(scan(b"\x1b]133;A"), []);
        // Cancelled.
        assert_eq!(scan(b"\x1b]133;A\x18\x07"), []);
    }

    #[test]
    fn long_sequences_are_skipped() {
        let mut bytes = b"\x1b]52;c;".to_vec();
        bytes.extend(vec![b'A'; 10_000]);
        bytes.extend_from_slice(b"\x07\x1b]133;C\x07");
        assert_eq!(scan(&bytes), [Marker::CommandExecuted]);
    }

    #[test]
    fn text_around_markers() {
        let bytes = b"hello\r\n\x1b[1mworld\x1b[0m\x1b]133;D;1\x07\x1b]133;A\x07$ \x1b]133;B\x07ls\r\n\x1b]133;C\x07out";
        let want = [
            Marker::CommandFinished(Some(1)),
            Marker::PromptStart,
            Marker::CommandStart,
            Marker::CommandExecuted,
        ];
        assert_eq!(scan(bytes), want);
        for markers in scan_everywhere(bytes) {
            assert_eq!(markers, want);
        }
    }

    #[test]
    fn split_at_every_position() {
        for bytes in [
            &b"\x1b]133;D;12\x07"[..],
            b"\x1b]133;D;12\x1b\\",
            b"\x1b]133;D;12;aid=3\x07",
        ] {
            for markers in scan_everywhere(bytes) {
                assert_eq!(markers, [Marker::CommandFinished(Some(12))]);
            }
        }
        for markers in scan_everywhere(b"\x1b]133;A\x1b\\\x1b]133;C\x07") {
            assert_eq!(markers, [Marker::PromptStart, Marker::CommandExecuted]);
        }
    }

    #[test]
    fn osc_restarted_by_escape() {
        assert_eq!(scan(b"\x1b]0;ti\x1b]133;A\x07"), [Marker::PromptStart]);
        assert_eq!(scan(b"\x1b]133;A\x1b[0m\x07"), []);
    }
}
