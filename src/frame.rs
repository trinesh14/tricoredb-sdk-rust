//! Frame layout: `version: u8 | tag: u8 | payload_len: u32 (big-endian) | payload`.
//!
//! Size ceilings are enforced in both directions and before a payload byte is
//! read, so a hostile or wrong-port peer cannot make this client allocate a
//! declared 4 GiB payload.

use std::io::{Read, Write};

use crate::error::{Error, ErrorKind, Result};

/// The frame header format version this client writes and the highest it reads.
pub const FRAME_VERSION: u8 = 1;

/// Length of the fixed frame header in bytes.
pub const HEADER_LEN: usize = 6;

/// Payload ceiling for `REQUEST` and `RESPONSE` frames (16 MiB).
pub const MAX_DATA_PAYLOAD: usize = 16 * 1024 * 1024;

/// Payload ceiling for every other (control) frame (64 KiB).
pub const MAX_CONTROL_PAYLOAD: usize = 64 * 1024;

/// A frame tag as it appears on the wire.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum Tag {
    /// Client handshake request.
    Hello,
    /// Client credentials.
    Auth,
    /// Client request envelope.
    Request,
    /// Server response envelope.
    Response,
    /// Client liveness probe.
    Ping,
    /// Server answer to `Ping`.
    Pong,
    /// Server connection-level refusal.
    Error,
    /// Client asks to end the session.
    Close,
    /// Server answer to `Hello`.
    HelloOk,
    /// Server answer to `Auth`, successful or not.
    AuthOk,
    /// Server acknowledgement of `Close`.
    Bye,
    /// Client asks the server to stop a running statement.
    Cancel,
    /// Server answer to `Cancel`.
    CancelOk,
}

impl Tag {
    /// The wire byte for this tag.
    pub fn as_u8(self) -> u8 {
        match self {
            Tag::Hello => 0,
            Tag::Auth => 1,
            Tag::Request => 2,
            Tag::Response => 3,
            Tag::Ping => 4,
            Tag::Pong => 5,
            Tag::Error => 6,
            Tag::Close => 7,
            Tag::HelloOk => 8,
            Tag::AuthOk => 9,
            Tag::Bye => 10,
            Tag::Cancel => 11,
            Tag::CancelOk => 12,
        }
    }

    /// Parse a wire byte, or `None` for a tag this client does not know.
    pub fn from_u8(b: u8) -> Option<Tag> {
        Some(match b {
            0 => Tag::Hello,
            1 => Tag::Auth,
            2 => Tag::Request,
            3 => Tag::Response,
            4 => Tag::Ping,
            5 => Tag::Pong,
            6 => Tag::Error,
            7 => Tag::Close,
            8 => Tag::HelloOk,
            9 => Tag::AuthOk,
            10 => Tag::Bye,
            11 => Tag::Cancel,
            12 => Tag::CancelOk,
            _ => return None,
        })
    }

    /// The largest payload a frame with this tag may carry.
    pub fn max_payload(self) -> usize {
        match self {
            Tag::Request | Tag::Response => MAX_DATA_PAYLOAD,
            _ => MAX_CONTROL_PAYLOAD,
        }
    }
}

/// One decoded frame.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Frame {
    /// The frame's tag.
    pub tag: Tag,
    /// The raw payload (UTF-8 JSON, or empty).
    pub payload: Vec<u8>,
}

/// Encode a frame into bytes, refusing a payload above the tag's ceiling.
pub fn encode(tag: Tag, payload: &[u8]) -> Result<Vec<u8>> {
    let limit = tag.max_payload();
    if payload.len() > limit {
        return Err(Error::new(
            ErrorKind::Protocol,
            format!(
                "refusing to send a {}-byte {:?} frame: the protocol caps it at {} bytes",
                payload.len(),
                tag,
                limit
            ),
        ));
    }
    let mut out = Vec::with_capacity(HEADER_LEN + payload.len());
    out.push(FRAME_VERSION);
    out.push(tag.as_u8());
    out.extend_from_slice(&(payload.len() as u32).to_be_bytes());
    out.extend_from_slice(payload);
    Ok(out)
}

/// Write one frame and flush.
pub fn write_frame<W: Write + ?Sized>(w: &mut W, tag: Tag, payload: &[u8]) -> Result<()> {
    let bytes = encode(tag, payload)?;
    w.write_all(&bytes).map_err(Error::from_io)?;
    w.flush().map_err(Error::from_io)
}

/// Read one frame, validating version, tag and size before reading the payload.
pub fn read_frame<R: Read + ?Sized>(r: &mut R) -> Result<Frame> {
    let mut head = [0u8; HEADER_LEN];
    r.read_exact(&mut head).map_err(Error::from_io)?;
    let version = head[0];
    if version > FRAME_VERSION {
        return Err(Error::new(
            ErrorKind::Protocol,
            format!(
                "frame header version {version} is newer than this client can read (max {FRAME_VERSION})"
            ),
        ));
    }
    let tag = Tag::from_u8(head[1]).ok_or_else(|| {
        Error::new(
            ErrorKind::Protocol,
            format!("unknown frame tag {}", head[1]),
        )
    })?;
    let len = u32::from_be_bytes([head[2], head[3], head[4], head[5]]) as usize;
    if len > tag.max_payload() {
        return Err(Error::new(
            ErrorKind::Protocol,
            format!(
                "{:?} frame declares a {len}-byte payload, above its {}-byte limit",
                tag,
                tag.max_payload()
            ),
        ));
    }
    let mut payload = vec![0u8; len];
    r.read_exact(&mut payload).map_err(Error::from_io)?;
    Ok(Frame { tag, payload })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    #[test]
    fn every_tag_round_trips_through_its_byte() {
        for b in 0u8..=12 {
            let tag = Tag::from_u8(b).expect("known tag");
            assert_eq!(tag.as_u8(), b);
        }
        assert_eq!(Tag::from_u8(13), None);
        assert_eq!(Tag::from_u8(255), None);
    }

    #[test]
    fn header_layout_is_version_tag_big_endian_length() {
        let bytes = encode(Tag::Request, b"{}").unwrap();
        assert_eq!(bytes, vec![1, 2, 0, 0, 0, 2, b'{', b'}']);
        let empty = encode(Tag::Ping, b"").unwrap();
        assert_eq!(empty, vec![1, 4, 0, 0, 0, 0]);
    }

    #[test]
    fn a_frame_round_trips() {
        let mut buf = Vec::new();
        write_frame(&mut buf, Tag::AuthOk, br#"{"ok":false}"#).unwrap();
        let f = read_frame(&mut Cursor::new(buf)).unwrap();
        assert_eq!(f.tag, Tag::AuthOk);
        assert_eq!(f.payload, br#"{"ok":false}"#);
    }

    #[test]
    fn only_data_tags_take_the_large_ceiling() {
        assert_eq!(Tag::Request.max_payload(), 16 * 1024 * 1024);
        assert_eq!(Tag::Response.max_payload(), 16 * 1024 * 1024);
        for b in [0u8, 1, 4, 5, 6, 7, 8, 9, 10, 11, 12] {
            assert_eq!(Tag::from_u8(b).unwrap().max_payload(), 64 * 1024);
        }
    }

    #[test]
    fn an_oversized_control_frame_is_refused_on_write() {
        let big = vec![b'a'; MAX_CONTROL_PAYLOAD + 1];
        let err = encode(Tag::Auth, &big).unwrap_err();
        assert_eq!(err.kind, ErrorKind::Protocol);
        assert!(encode(Tag::Request, &big).is_ok());
    }

    #[test]
    fn an_oversized_declared_length_is_refused_before_reading_the_payload() {
        let mut head = vec![1u8, 9];
        head.extend_from_slice(&((MAX_CONTROL_PAYLOAD as u32) + 1).to_be_bytes());
        let err = read_frame(&mut Cursor::new(head)).unwrap_err();
        assert_eq!(err.kind, ErrorKind::Protocol);
        assert!(err.message.contains("limit"), "{}", err.message);

        let mut data = vec![1u8, 3];
        data.extend_from_slice(&((MAX_DATA_PAYLOAD as u32) + 1).to_be_bytes());
        assert_eq!(
            read_frame(&mut Cursor::new(data)).unwrap_err().kind,
            ErrorKind::Protocol
        );
    }

    #[test]
    fn a_newer_frame_version_is_refused() {
        let bytes = vec![2u8, 3, 0, 0, 0, 0];
        let err = read_frame(&mut Cursor::new(bytes)).unwrap_err();
        assert!(err.message.contains("version 2"), "{}", err.message);
    }

    #[test]
    fn an_unknown_tag_is_refused() {
        let bytes = vec![1u8, 42, 0, 0, 0, 0];
        let err = read_frame(&mut Cursor::new(bytes)).unwrap_err();
        assert!(
            err.message.contains("unknown frame tag 42"),
            "{}",
            err.message
        );
    }

    #[test]
    fn a_truncated_payload_is_an_io_error() {
        let bytes = vec![1u8, 3, 0, 0, 0, 10, b'{'];
        let err = read_frame(&mut Cursor::new(bytes)).unwrap_err();
        assert_eq!(err.kind, ErrorKind::Io);
    }
}
