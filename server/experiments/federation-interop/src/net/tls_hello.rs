//! Reads the negotiated TLS version and cipher suite from a ServerHello
//! the relay saw in clear (RFC 8446 section 4.1.3, RFC 5246 section
//! 7.4.1.3). Only the first handshake record is read; a ServerHello split
//! across records is reported as absent.

use serde::Serialize;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct ServerHello {
    /// `0x0304` for TLS 1.3, `0x0303` for TLS 1.2.
    pub version: u16,
    pub cipher_suite: u16,
}

impl ServerHello {
    pub fn version_name(&self) -> &'static str {
        match self.version {
            0x0304 => "TLS 1.3",
            0x0303 => "TLS 1.2",
            0x0302 => "TLS 1.1",
            0x0301 => "TLS 1.0",
            _ => "unknown",
        }
    }
}

const HANDSHAKE: u8 = 0x16;
const SERVER_HELLO: u8 = 0x02;
const SUPPORTED_VERSIONS: u16 = 43;

struct Cursor<'a> {
    bytes: &'a [u8],
}

impl<'a> Cursor<'a> {
    fn take(&mut self, count: usize) -> Option<&'a [u8]> {
        let (head, tail) = (self.bytes.get(..count)?, self.bytes.get(count..)?);
        self.bytes = tail;
        Some(head)
    }

    fn u8(&mut self) -> Option<u8> {
        self.take(1)?.first().copied()
    }

    fn u16(&mut self) -> Option<u16> {
        let bytes = self.take(2)?;
        Some(u16::from_be_bytes([*bytes.first()?, *bytes.get(1)?]))
    }

    fn u24(&mut self) -> Option<usize> {
        let bytes = self.take(3)?;
        Some(
            (usize::from(*bytes.first()?) << 16)
                | (usize::from(*bytes.get(1)?) << 8)
                | usize::from(*bytes.get(2)?),
        )
    }
}

/// Parse the ServerHello at the start of `bytes`, a TLS record stream.
pub fn parse_server_hello(bytes: &[u8]) -> Option<ServerHello> {
    let mut record = Cursor { bytes };
    if record.u8()? != HANDSHAKE {
        return None;
    }
    let _legacy_record_version = record.u16()?;
    let length = usize::from(record.u16()?);
    let mut handshake = Cursor {
        bytes: record.take(length)?,
    };
    if handshake.u8()? != SERVER_HELLO {
        return None;
    }
    let body_length = handshake.u24()?;
    let mut body = Cursor {
        bytes: handshake.take(body_length)?,
    };
    let legacy_version = body.u16()?;
    let _random = body.take(32)?;
    let session_id_length = usize::from(body.u8()?);
    let _session_id = body.take(session_id_length)?;
    let cipher_suite = body.u16()?;
    let _compression = body.u8()?;
    let mut version = legacy_version;
    if let Some(extensions_length) = body.u16() {
        let mut extensions = Cursor {
            bytes: body.take(usize::from(extensions_length))?,
        };
        while let Some(kind) = extensions.u16() {
            let length = usize::from(extensions.u16()?);
            let mut data = Cursor {
                bytes: extensions.take(length)?,
            };
            if kind == SUPPORTED_VERSIONS {
                version = data.u16()?;
            }
        }
    }
    Some(ServerHello {
        version,
        cipher_suite,
    })
}
