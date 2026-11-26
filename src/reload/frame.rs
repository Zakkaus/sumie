//! Frames the helper children's messages (the configuration child's results, the pasteboard
//! child's request and reply): a 4-byte big-endian body length, a version byte, a kind byte,
//! then the body. The decoder validates the header before it allocates the body,
//! accepts input split at any byte, and reports a frame only once it is complete.

use std::fmt;

pub(crate) const VERSION: u8 = 1;
/// The largest body either side accepts.
pub(crate) const MAX_BODY: usize = 1 << 20;
const HEADER: usize = 6;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Kind {
    /// Resolved settings as `sumie_config::Config::to_json`.
    Settings = 1,
    /// A located diagnostic for the banner, as UTF-8 text.
    Invalid = 2,
    /// Pasteboard requests: read the string, replace it with the UTF-8 body, or clear it.
    Read = 3,
    Write = 4,
    Clear = 5,
    /// Pasteboard replies: the string as UTF-8, no string, a string over the read limit, a
    /// write the board took, or one it refused.
    Text = 6,
    Empty = 7,
    TooLarge = 8,
    Written = 9,
    Refused = 10,
}

impl Kind {
    const ALL: [Self; 10] = [
        Self::Settings,
        Self::Invalid,
        Self::Read,
        Self::Write,
        Self::Clear,
        Self::Text,
        Self::Empty,
        Self::TooLarge,
        Self::Written,
        Self::Refused,
    ];
}

#[derive(Debug, PartialEq, Eq)]
pub(crate) struct Frame {
    pub(crate) kind: Kind,
    pub(crate) body: Vec<u8>,
}

#[derive(Debug, PartialEq, Eq)]
pub(crate) enum FrameError {
    Version(u8),
    Kind(u8),
    TooLong(u32),
    /// The stream ended inside a frame after this many of its bytes.
    Truncated(usize),
}

impl fmt::Display for FrameError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Version(version) => write!(formatter, "unsupported frame version {version}"),
            Self::Kind(kind) => write!(formatter, "unknown frame kind {kind}"),
            Self::TooLong(length) => {
                write!(formatter, "frame body of {length} bytes exceeds {MAX_BODY}")
            }
            Self::Truncated(read) => write!(formatter, "stream ended {read} bytes into a frame"),
        }
    }
}

impl std::error::Error for FrameError {}

/// Encode one frame.
///
/// # Panics
/// Panics if `body` exceeds [`MAX_BODY`].
pub(crate) fn encode(kind: Kind, body: &[u8]) -> Vec<u8> {
    assert!(body.len() <= MAX_BODY, "frame body exceeds the cap");
    let length = u32::try_from(body.len()).expect("capped body length");
    let mut frame = Vec::with_capacity(HEADER + body.len());
    frame.extend_from_slice(&length.to_be_bytes());
    frame.extend_from_slice(&[VERSION, kind as u8]);
    frame.extend_from_slice(body);
    frame
}

/// Incremental decoder. After an error it must not be fed again.
#[derive(Default)]
pub(crate) struct Decoder {
    header: [u8; HEADER],
    filled: usize,
    body: Option<(Kind, Vec<u8>, usize)>,
}

impl Decoder {
    /// Append `input` and push every frame it completes.
    pub(crate) fn feed(
        &mut self,
        mut input: &[u8],
        frames: &mut Vec<Frame>,
    ) -> Result<(), FrameError> {
        loop {
            if let Some((kind, body, length)) = &mut self.body {
                let take = (*length - body.len()).min(input.len());
                body.extend_from_slice(&input[..take]);
                input = &input[take..];
                if body.len() < *length {
                    return Ok(());
                }
                frames.push(Frame {
                    kind: *kind,
                    body: std::mem::take(body),
                });
                self.body = None;
                self.filled = 0;
            }
            if input.is_empty() {
                return Ok(());
            }
            let take = (HEADER - self.filled).min(input.len());
            self.header[self.filled..][..take].copy_from_slice(&input[..take]);
            self.filled += take;
            input = &input[take..];
            if self.filled < HEADER {
                return Ok(());
            }
            let (kind, length) = self.parse_header()?;
            self.body = Some((kind, Vec::with_capacity(length), length));
        }
    }

    /// Check the stream ended on a frame boundary.
    pub(crate) fn finish(&self) -> Result<(), FrameError> {
        let read = self.filled + self.body.as_ref().map_or(0, |(_, body, _)| body.len());
        if read == 0 {
            Ok(())
        } else {
            Err(FrameError::Truncated(read))
        }
    }

    /// Validate the complete header, including the cap, before anything is reserved.
    fn parse_header(&self) -> Result<(Kind, usize), FrameError> {
        let [a, b, c, d, version, kind] = self.header;
        if version != VERSION {
            return Err(FrameError::Version(version));
        }
        let Some(kind) = Kind::ALL.into_iter().find(|known| *known as u8 == kind) else {
            return Err(FrameError::Kind(kind));
        };
        let length = u32::from_be_bytes([a, b, c, d]);
        let size = usize::try_from(length).map_err(|_| FrameError::TooLong(length))?;
        if size > MAX_BODY {
            return Err(FrameError::TooLong(length));
        }
        Ok((kind, size))
    }

    #[cfg(test)]
    fn reserved(&self) -> usize {
        self.body.as_ref().map_or(0, |(_, body, _)| body.capacity())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn decode(chunks: &[&[u8]]) -> (Vec<Frame>, Result<(), FrameError>) {
        let mut decoder = Decoder::default();
        let mut frames = Vec::new();
        for chunk in chunks {
            if let Err(error) = decoder.feed(chunk, &mut frames) {
                return (frames, Err(error));
            }
        }
        let end = decoder.finish();
        (frames, end)
    }

    #[test]
    fn split_reads_yield_whole_frames_only() {
        let mut stream = encode(Kind::Settings, b"{\"font\":{}}");
        stream.extend(encode(Kind::Invalid, b""));
        stream.extend(encode(Kind::Invalid, "sumie.toml:2:1: 墨".as_bytes()));
        let expected = decode(&[&stream]).0;
        assert_eq!(expected.len(), 3);
        assert_eq!(expected[1].body, b"");
        for split in 1..stream.len() {
            for second in split..stream.len() {
                let chunks = [&stream[..split], &stream[split..second], &stream[second..]];
                assert_eq!(
                    decode(&chunks),
                    (decode(&[&stream]).0, Ok(())),
                    "{split}/{second}"
                );
            }
        }
        // One byte at a time: nothing is reported before its last byte arrives.
        let mut decoder = Decoder::default();
        let mut frames = Vec::new();
        let first = encode(Kind::Settings, b"{\"font\":{}}").len();
        for (index, byte) in stream.iter().enumerate() {
            decoder.feed(&[*byte], &mut frames).unwrap();
            assert_eq!(frames.is_empty(), index + 1 < first);
        }
        assert_eq!(frames, expected);
    }

    #[test]
    fn a_body_over_the_cap_is_rejected_before_allocation() {
        let body = vec![b'x'; MAX_BODY];
        let (frames, end) = decode(&[&encode(Kind::Settings, &body)]);
        assert_eq!((frames.len(), end), (1, Ok(())));

        let over = u32::try_from(MAX_BODY + 1).unwrap();
        for length in [over, u32::MAX] {
            let mut decoder = Decoder::default();
            let mut header = length.to_be_bytes().to_vec();
            header.extend([VERSION, Kind::Settings as u8]);
            let result = decoder.feed(&header, &mut Vec::new());
            assert_eq!(
                (result, decoder.reserved()),
                (Err(FrameError::TooLong(length)), 0),
                "the cap must be checked before the body is reserved"
            );
        }
    }

    #[test]
    fn a_half_frame_at_eof_is_truncated() {
        let frame = encode(Kind::Invalid, b"sumie.toml:1:1: bad");
        let (frames, end) = decode(&[&frame, &frame[..3]]);
        assert_eq!((frames.len(), end), (1, Err(FrameError::Truncated(3))));
        let (frames, end) = decode(&[&frame[..frame.len() - 1]]);
        assert_eq!(
            (frames.len(), end),
            (0, Err(FrameError::Truncated(frame.len() - 1)))
        );
        assert_eq!(decode(&[]), (Vec::new(), Ok(())));
    }

    #[test]
    fn a_bad_version_or_kind_is_rejected() {
        let mut frame = encode(Kind::Settings, b"{}");
        frame[4] = VERSION + 1;
        assert_eq!(decode(&[&frame]).1, Err(FrameError::Version(VERSION + 1)));
        frame[4] = VERSION;
        frame[5] = 11;
        assert_eq!(decode(&[&frame]).1, Err(FrameError::Kind(11)));
    }
}
