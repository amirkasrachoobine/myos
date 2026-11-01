//! Length-prefixed framing: a 4-byte big-endian length, then a serialized
//! `Frame`. Shared by the broker and the library.

use crate::proto::Frame;
use prost::Message;
use std::io::{self, Read, Write};

/// Largest frame either side accepts. Bigger ones are a protocol error.
pub const MAX_FRAME: usize = 16 << 20;

/// Reads one frame. `Ok(None)` means the peer closed the connection cleanly
/// between frames.
pub fn read_frame(r: &mut impl Read) -> io::Result<Option<Frame>> {
    let mut len = [0u8; 4];
    // Distinguish a clean close (no bytes at all) from a truncated header.
    let mut got = 0;
    while got < 4 {
        match r.read(&mut len[got..]) {
            Ok(0) if got == 0 => return Ok(None),
            Ok(0) => return Err(io::ErrorKind::UnexpectedEof.into()),
            Ok(n) => got += n,
            Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
            Err(e) => return Err(e),
        }
    }
    let len = u32::from_be_bytes(len) as usize;
    if len > MAX_FRAME {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("frame of {len} bytes exceeds the {MAX_FRAME}-byte limit"),
        ));
    }
    let mut buf = vec![0u8; len];
    r.read_exact(&mut buf)?;
    Frame::decode(buf.as_slice())
        .map(Some)
        .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, format!("bad frame: {e}")))
}

/// Writes one frame in a single `write_all`, so frames written by different
/// threads under a lock never interleave.
pub fn write_frame(w: &mut impl Write, frame: &Frame) -> io::Result<()> {
    let len = frame.encoded_len();
    if len > MAX_FRAME {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("frame of {len} bytes exceeds the {MAX_FRAME}-byte limit"),
        ));
    }
    let mut buf = Vec::with_capacity(4 + len);
    buf.extend_from_slice(&(len as u32).to_be_bytes());
    frame.encode(&mut buf).expect("Vec has capacity");
    w.write_all(&buf)?;
    w.flush()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::proto::{frame::Kind, Call};

    #[test]
    fn round_trip() {
        let f = Frame {
            id: 7,
            kind: Some(Kind::Call(Call { service: "bus".into(), method: "Ping".into(), ..Default::default() })),
        };
        let mut buf = Vec::new();
        write_frame(&mut buf, &f).unwrap();
        write_frame(&mut buf, &f).unwrap();
        let mut r = buf.as_slice();
        assert_eq!(read_frame(&mut r).unwrap(), Some(f.clone()));
        assert_eq!(read_frame(&mut r).unwrap(), Some(f));
        assert_eq!(read_frame(&mut r).unwrap(), None);
    }

    #[test]
    fn rejects_oversized_and_truncated() {
        let big = ((MAX_FRAME + 1) as u32).to_be_bytes();
        assert_eq!(read_frame(&mut &big[..]).unwrap_err().kind(), io::ErrorKind::InvalidData);
        assert_eq!(read_frame(&mut &[0u8, 0][..]).unwrap_err().kind(), io::ErrorKind::UnexpectedEof);
        assert_eq!(read_frame(&mut &[0u8, 0, 0, 5, 1][..]).unwrap_err().kind(), io::ErrorKind::UnexpectedEof);
    }
}
