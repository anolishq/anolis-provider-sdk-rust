//! framed-stdio v1: a `uint32` little-endian length prefix, then that many
//! bytes of protobuf, capped at 1 MiB in both directions (anolis-protocol
//! `docs/profiles/framed-stdio-v1.md`).

use std::fmt;
use std::io::{self, ErrorKind, Read, Write};

/// The largest frame either side may send (framed-stdio v1 §2).
pub const MAX_FRAME_BYTES: usize = 1024 * 1024;

/// The byte stream is not a valid frame sequence, or it could not be read or written.
#[derive(Debug)]
pub enum FrameError {
    /// The stream ended part-way through a frame.
    Truncated {
        got: usize,
        want: usize,
    },
    /// A frame longer than [`MAX_FRAME_BYTES`].
    TooLarge(usize),
    Io(io::Error),
}

impl fmt::Display for FrameError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Truncated { got, want } => write!(f, "stream ended after {got} of {want} bytes"),
            Self::TooLarge(len) => write!(f, "frame of {len} bytes exceeds the {MAX_FRAME_BYTES}-byte limit"),
            Self::Io(err) => write!(f, "{err}"),
        }
    }
}

impl std::error::Error for FrameError {}

/// Fill `buf`. Returns the number of bytes read, which is short only at EOF.
fn read_full(input: &mut impl Read, buf: &mut [u8]) -> Result<usize, FrameError> {
    let mut filled = 0;
    while filled < buf.len() {
        match input.read(&mut buf[filled..]) {
            Ok(0) => break,
            Ok(n) => filled += n,
            Err(err) if err.kind() == ErrorKind::Interrupted => {}
            Err(err) => return Err(FrameError::Io(err)),
        }
    }
    Ok(filled)
}

/// The next frame's payload, or `None` on a clean EOF between frames.
pub fn read_frame(input: &mut impl Read) -> Result<Option<Vec<u8>>, FrameError> {
    let mut prefix = [0u8; 4];
    match read_full(input, &mut prefix)? {
        0 => return Ok(None),
        4 => {}
        got => return Err(FrameError::Truncated { got, want: 4 }),
    }
    let len = u32::from_le_bytes(prefix) as usize;
    if len > MAX_FRAME_BYTES {
        return Err(FrameError::TooLarge(len));
    }
    let mut payload = vec![0u8; len];
    let got = read_full(input, &mut payload)?;
    if got < len {
        return Err(FrameError::Truncated { got, want: len });
    }
    Ok(Some(payload))
}

/// Write one frame and flush it.
pub fn write_frame(output: &mut impl Write, payload: &[u8]) -> Result<(), FrameError> {
    if payload.len() > MAX_FRAME_BYTES {
        return Err(FrameError::TooLarge(payload.len()));
    }
    let prefix = (payload.len() as u32).to_le_bytes();
    output.write_all(&prefix).map_err(FrameError::Io)?;
    output.write_all(payload).map_err(FrameError::Io)?;
    output.flush().map_err(FrameError::Io)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn frame(payload: &[u8]) -> Vec<u8> {
        let mut out = Vec::new();
        write_frame(&mut out, payload).unwrap();
        out
    }

    /// Hands out at most one byte per read, so every frame arrives fragmented.
    struct Drip<'a>(&'a [u8]);

    impl Read for Drip<'_> {
        fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
            if self.0.is_empty() || buf.is_empty() {
                return Ok(0);
            }
            buf[0] = self.0[0];
            self.0 = &self.0[1..];
            Ok(1)
        }
    }

    #[test]
    fn round_trips_coalesced_frames() {
        let mut stream = frame(b"one");
        stream.extend(frame(b""));
        stream.extend(frame(b"three"));
        let mut input = stream.as_slice();
        assert_eq!(read_frame(&mut input).unwrap().unwrap(), b"one");
        assert_eq!(read_frame(&mut input).unwrap().unwrap(), b"");
        assert_eq!(read_frame(&mut input).unwrap().unwrap(), b"three");
        assert!(read_frame(&mut input).unwrap().is_none());
    }

    #[test]
    fn reassembles_fragmented_frames() {
        let stream = frame(b"fragmented");
        assert_eq!(read_frame(&mut Drip(&stream)).unwrap().unwrap(), b"fragmented");
    }

    #[test]
    fn little_endian_prefix() {
        assert_eq!(&frame(&[7; 258])[..4], &[2, 1, 0, 0]);
    }

    #[test]
    fn truncated_prefix_and_payload_are_errors() {
        assert!(matches!(read_frame(&mut &[1u8, 0][..]), Err(FrameError::Truncated { got: 2, want: 4 })));
        let mut short = frame(b"payload");
        short.truncate(6);
        assert!(matches!(read_frame(&mut short.as_slice()), Err(FrameError::Truncated { got: 2, want: 7 })));
    }

    #[test]
    fn oversize_prefix_is_refused_before_reading() {
        let prefix = ((MAX_FRAME_BYTES + 1) as u32).to_le_bytes();
        assert!(matches!(read_frame(&mut &prefix[..]), Err(FrameError::TooLarge(n)) if n == MAX_FRAME_BYTES + 1));
        let at_limit = (MAX_FRAME_BYTES as u32).to_le_bytes();
        assert!(matches!(read_frame(&mut &at_limit[..]), Err(FrameError::Truncated { got: 0, .. })));
    }

    #[test]
    fn oversize_write_is_refused() {
        let mut out = Vec::new();
        assert!(matches!(write_frame(&mut out, &vec![0; MAX_FRAME_BYTES + 1]), Err(FrameError::TooLarge(_))));
        assert!(out.is_empty());
    }
}
