//! The postcard frame codec skeleton.
//!
//! Wire format is deliberately boring: a 4-byte big-endian body length
//! followed by a postcard-serialised [`Frame`]. Length-prefixing keeps a
//! corrupt or truncated stream from being silently re-synchronised mid-record
//! -- the Python engine's "turning one refused object into a desynchronised
//! batch" hazard, refused here as [`BulkloadRefusal::FrameCodec`].

use serde::{Deserialize, Serialize};

use crate::refusal::BulkloadRefusal;
use crate::row::RowSchema;
use crate::Result;

/// Wire protocol version. Bump on any incompatible [`Frame`] change.
pub const PROTO_VERSION: u16 = 1;

/// Bytes of frame header carrying the body length.
pub const LENGTH_PREFIX_BYTES: usize = 4;

/// Largest body this codec will encode or accept, in bytes.
///
/// A frame carries one row or one small control record; anything larger is a
/// desynchronised stream, not a big row.
pub const MAX_FRAME_BYTES: usize = 8 * 1024 * 1024;

/// The payload a frame carries.
#[non_exhaustive]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum FrameKind {
    /// Opens a stream and names the corpus root as raw OS bytes.
    Hello { corpus_root: Vec<u8> },
    /// One scanned filesystem seat.
    Row(RowSchema),
    /// The agent declined a seat, naming the refusal code.
    Refusal { code: String, rel_path: Vec<u8> },
    /// Closes a stream. `rows` is the count the sender believes it emitted.
    Done { rows: u64 },
}

/// One framed protocol message.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Frame {
    /// Protocol version the sender encoded with.
    pub version: u16,
    /// The payload.
    pub kind: FrameKind,
}

impl Frame {
    /// Build a frame at the current [`PROTO_VERSION`].
    #[must_use]
    pub const fn new(kind: FrameKind) -> Self {
        Self {
            version: PROTO_VERSION,
            kind,
        }
    }

    /// Encode to a length-prefixed postcard frame.
    ///
    /// # Errors
    ///
    /// Refuses with [`BulkloadRefusal::FrameCodec`] if postcard declines the
    /// value, and [`BulkloadRefusal::BudgetExceeded`] if the body would exceed
    /// [`MAX_FRAME_BYTES`].
    pub fn encode(&self) -> Result<Vec<u8>> {
        let body = postcard::to_stdvec(self)?;
        if body.len() > MAX_FRAME_BYTES {
            return Err(BulkloadRefusal::BudgetExceeded);
        }
        let len = u32::try_from(body.len()).map_err(|_| BulkloadRefusal::BudgetExceeded)?;
        let mut out = Vec::with_capacity(LENGTH_PREFIX_BYTES + body.len());
        out.extend_from_slice(&len.to_be_bytes());
        out.extend_from_slice(&body);
        Ok(out)
    }

    /// Decode one frame from the front of `buf`.
    ///
    /// Returns the frame and the number of bytes consumed, so a caller can
    /// drive a stream without re-scanning for a delimiter.
    ///
    /// # Errors
    ///
    /// Refuses with [`BulkloadRefusal::FrameCodec`] on a truncated header, a
    /// truncated body, a postcard decode failure, or a version this build does
    /// not speak; [`BulkloadRefusal::BudgetExceeded`] if the declared body
    /// length exceeds [`MAX_FRAME_BYTES`].
    pub fn decode(buf: &[u8]) -> Result<(Self, usize)> {
        let header: [u8; LENGTH_PREFIX_BYTES] = buf
            .get(..LENGTH_PREFIX_BYTES)
            .ok_or(BulkloadRefusal::FrameCodec)?
            .try_into()
            .map_err(|_| BulkloadRefusal::FrameCodec)?;
        let body_len = usize::try_from(u32::from_be_bytes(header))
            .map_err(|_| BulkloadRefusal::BudgetExceeded)?;
        if body_len > MAX_FRAME_BYTES {
            return Err(BulkloadRefusal::BudgetExceeded);
        }
        let end = LENGTH_PREFIX_BYTES
            .checked_add(body_len)
            .ok_or(BulkloadRefusal::BudgetExceeded)?;
        let body = buf
            .get(LENGTH_PREFIX_BYTES..end)
            .ok_or(BulkloadRefusal::FrameCodec)?;
        let frame: Self = postcard::from_bytes(body)?;
        if frame.version != PROTO_VERSION {
            return Err(BulkloadRefusal::FrameCodec);
        }
        Ok((frame, end))
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::panic)]

    use super::{Frame, FrameKind, BulkloadRefusal, MAX_FRAME_BYTES};

    fn sample() -> Frame {
        Frame::new(FrameKind::Hello {
            corpus_root: b"/srv/corpus".to_vec(),
        })
    }

    #[test]
    fn round_trips() {
        let frame = sample();
        let bytes = frame.encode().unwrap();
        let (decoded, consumed) = Frame::decode(&bytes).unwrap();
        assert_eq!(decoded, frame);
        assert_eq!(consumed, bytes.len());
    }

    #[test]
    fn decodes_back_to_back_frames() {
        let mut stream = sample().encode().unwrap();
        let second = Frame::new(FrameKind::Done { rows: 7 });
        stream.extend_from_slice(&second.encode().unwrap());

        let (first, consumed) = Frame::decode(&stream).unwrap();
        assert_eq!(first, sample());
        let (tail, _) = Frame::decode(stream.get(consumed..).unwrap()).unwrap();
        assert_eq!(tail, second);
    }

    #[test]
    fn refuses_truncated_header_and_body() {
        let bytes = sample().encode().unwrap();
        assert_eq!(
            Frame::decode(&[0_u8, 0]).unwrap_err(),
            BulkloadRefusal::FrameCodec
        );
        let truncated = bytes.get(..bytes.len() - 1).unwrap();
        assert_eq!(
            Frame::decode(truncated).unwrap_err(),
            BulkloadRefusal::FrameCodec
        );
    }

    #[test]
    fn refuses_oversized_declared_length() {
        let len = u32::try_from(MAX_FRAME_BYTES + 1).unwrap();
        let mut bytes = len.to_be_bytes().to_vec();
        bytes.push(0);
        assert_eq!(
            Frame::decode(&bytes).unwrap_err(),
            BulkloadRefusal::BudgetExceeded
        );
    }
}
