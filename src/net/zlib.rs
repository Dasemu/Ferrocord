//! `compress=zlib-stream` transport decompression.
//!
//! With transport compression the gateway sends binary frames that are
//! chunks of one long zlib stream shared by the whole connection. A complete
//! message ends with the 4-byte `Z_SYNC_FLUSH` suffix `00 00 ff ff`; partial
//! frames are buffered until the suffix arrives. One inflater lives exactly
//! as long as one WebSocket connection.

use flate2::{Decompress, FlushDecompress, Status};

use crate::error::{Error, Result};

const SUFFIX: [u8; 4] = [0x00, 0x00, 0xff, 0xff];
/// Refuse to buffer more than this much compressed data for one message.
const MAX_BUFFER: usize = 64 * 1024 * 1024;

pub struct ZlibStream {
    inflater: Decompress,
    buffer: Vec<u8>,
}

impl Default for ZlibStream {
    fn default() -> Self {
        Self::new()
    }
}

impl ZlibStream {
    pub fn new() -> Self {
        Self {
            inflater: Decompress::new(true),
            buffer: Vec::new(),
        }
    }

    /// Feeds one binary frame. Returns the decoded JSON text once a full
    /// message has been received.
    pub fn push(&mut self, frame: &[u8]) -> Result<Option<String>> {
        self.buffer.extend_from_slice(frame);
        if self.buffer.len() > MAX_BUFFER {
            self.buffer.clear();
            return Err(Error::Protocol("zlib message too large".into()));
        }
        if self.buffer.len() < 4 || self.buffer[self.buffer.len() - 4..] != SUFFIX {
            return Ok(None);
        }
        let input = std::mem::take(&mut self.buffer);
        let mut out: Vec<u8> = Vec::with_capacity(input.len() * 4 + 1024);
        let mut offset = 0;
        loop {
            if out.capacity() - out.len() < 16 * 1024 {
                out.reserve(64 * 1024);
            }
            let in_before = self.inflater.total_in();
            let out_before = self.inflater.total_out();
            let status = self
                .inflater
                .decompress_vec(&input[offset..], &mut out, FlushDecompress::Sync)
                .map_err(|e| Error::Protocol(format!("zlib: {e}")))?;
            let consumed = (self.inflater.total_in() - in_before) as usize;
            let produced = self.inflater.total_out() - out_before;
            offset += consumed;
            let spare = out.capacity() - out.len();
            if matches!(status, Status::StreamEnd)
                || (offset >= input.len() && spare > 0)
                || (consumed == 0 && produced == 0)
            {
                break;
            }
        }
        String::from_utf8(out)
            .map(Some)
            .map_err(|_| Error::Protocol("zlib payload is not UTF-8".into()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use flate2::{Compress, Compression, FlushCompress};

    /// Compresses messages the way the gateway does: one shared stream,
    /// `Z_SYNC_FLUSH` after every message.
    fn compress_all(msgs: &[&str]) -> Vec<Vec<u8>> {
        let mut c = Compress::new(Compression::default(), true);
        msgs.iter()
            .map(|m| {
                let mut out = Vec::with_capacity(m.len() + 64);
                c.compress_vec(m.as_bytes(), &mut out, FlushCompress::Sync)
                    .unwrap();
                out
            })
            .collect()
    }

    #[test]
    fn decodes_consecutive_messages_on_shared_context() {
        let msgs = [
            r#"{"op":10,"d":{"heartbeat_interval":41250}}"#,
            r#"{"op":11}"#,
            r#"{"op":0,"t":"MESSAGE_CREATE","s":3,"d":{"id":"1"}}"#,
        ];
        let frames = compress_all(&msgs);
        let mut z = ZlibStream::new();
        for (f, m) in frames.iter().zip(msgs) {
            assert!(f.ends_with(&SUFFIX));
            assert_eq!(z.push(f).unwrap().as_deref(), Some(m));
        }
    }

    #[test]
    fn buffers_split_frames() {
        let msg = "x".repeat(10_000) + "end";
        let frames = compress_all(&[&msg]);
        let f = &frames[0];
        let (a, b) = f.split_at(f.len() / 2);
        let mut z = ZlibStream::new();
        assert_eq!(z.push(a).unwrap(), None);
        assert_eq!(z.push(b).unwrap().as_deref(), Some(msg.as_str()));
    }

    #[test]
    fn large_payload_expands_beyond_initial_capacity() {
        // Highly compressible: output is much larger than 4x input.
        let msg = format!("{{\"d\":\"{}\"}}", "a".repeat(2_000_000));
        let frames = compress_all(&[&msg]);
        let mut z = ZlibStream::new();
        assert_eq!(z.push(&frames[0]).unwrap().unwrap().len(), msg.len());
    }

    #[test]
    fn garbage_is_an_error_not_a_panic() {
        let mut z = ZlibStream::new();
        assert!(z.push(&[1, 2, 3, 4, 0, 0, 0xff, 0xff]).is_err());
    }
}
