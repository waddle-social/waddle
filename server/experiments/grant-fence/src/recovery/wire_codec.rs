//! Opaque bytes exist only at this bounded gRPC wire forwarding boundary.
//! Tonic retains framing and validates the actual unary response status.
use bytes::{Buf, BufMut, Bytes};
use tonic::codec::{Codec, DecodeBuf, Decoder, EncodeBuf, Encoder};
use tonic::Status;

pub const MAX_MESSAGE_BYTES: usize = 8192;
#[derive(Clone, Default)]
pub struct WireCodec;
impl Codec for WireCodec {
    type Encode = Bytes;
    type Decode = Bytes;
    type Encoder = Self;
    type Decoder = Self;
    fn encoder(&mut self) -> Self {
        Self
    }
    fn decoder(&mut self) -> Self {
        Self
    }
}
impl Encoder for WireCodec {
    type Item = Bytes;
    type Error = Status;
    fn encode(&mut self, item: Bytes, target: &mut EncodeBuf<'_>) -> Result<(), Status> {
        if item.len() > MAX_MESSAGE_BYTES {
            return Err(Status::resource_exhausted("wire size"));
        }
        target.put(item);
        Ok(())
    }
}
impl Decoder for WireCodec {
    type Item = Bytes;
    type Error = Status;
    fn decode(&mut self, source: &mut DecodeBuf<'_>) -> Result<Option<Bytes>, Status> {
        if source.remaining() > MAX_MESSAGE_BYTES {
            return Err(Status::resource_exhausted("wire size"));
        }
        Ok(Some(source.copy_to_bytes(source.remaining())))
    }
}
