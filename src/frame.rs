use crate::opcode::Opcode;
use crate::{Error, Result};
use bytecodec::bytes::{BytesEncoder, CopyableBytesDecoder};
use bytecodec::combinator::Slice;
use bytecodec::io::StreamState;
use bytecodec::{ByteCount, Decode, Encode, Eos};
use byteorder::{BigEndian, ByteOrder};
use std::cmp;
use std::io::{self, Read, Write};

const FIN_FLAG: u8 = 0b1000_0000;
const MASK_FLAG: u8 = 0b1000_0000;

const BUF_SIZE: usize = 4096;

#[derive(Debug)]
pub enum Frame {
    ConnectionClose { code: u16, reason: Vec<u8> },
    Ping { data: Vec<u8> },
    Pong { data: Vec<u8> },
    Data,
}

#[derive(Debug, Clone)]
struct FrameHeader {
    _fin_flag: bool,
    opcode: Opcode,
    mask: Option<[u8; 4]>,
    payload_len: u64,
}
impl FrameHeader {
    fn from_bytes(b: [u8; 2]) -> bytecodec::Result<Self> {
        let mut header = FrameHeader {
            _fin_flag: (b[0] & FIN_FLAG) != 0,
            opcode: track!(Opcode::from_u8(b[0] & 0b1111))?,
            mask: None,
            payload_len: u64::from(b[1] & 0b0111_1111),
        };

        let mask_flag = (b[1] & MASK_FLAG) != 0;
        if mask_flag {
            header.mask = Some([0; 4]); // dummy
        }
        Ok(header)
    }
}

#[derive(Debug)]
pub struct FrameEncoder {
    header: Slice<BytesEncoder<[u8; 2 + 8]>>,
    payload: Vec<u8>,
    payload_offset: usize,
    payload_length: usize,
}
impl FrameEncoder {
    pub fn start_encoding_data<R: Read>(&mut self, mut reader: R) -> Result<StreamState> {
        if !self.is_idle() {
            return Ok(StreamState::Normal);
        }

        match reader.read(&mut self.payload) {
            Err(e) => {
                if e.kind() == io::ErrorKind::WouldBlock {
                    return Ok(StreamState::WouldBlock);
                } else {
                    return Err(track!(Error::from(e)));
                }
            }
            Ok(0) => return Ok(StreamState::Eos),
            Ok(size) => {
                track!(self.start_encoding_header(Opcode::BinaryFrame, size))?;
            }
        }
        Ok(StreamState::Normal)
    }

    fn start_encoding_header(
        &mut self,
        opcode: Opcode,
        payload_len: usize,
    ) -> bytecodec::Result<()> {
        let header_size;
        let mut header = [0; 2 + 8];
        header[0] = FIN_FLAG | (opcode as u8);
        if payload_len < 126 {
            header[1] = payload_len as u8;
            header_size = 2;
        } else if payload_len < 0x10000 {
            header[1] = 126;
            BigEndian::write_u16(&mut header[2..], payload_len as u16);
            header_size = 4;
        } else {
            header[1] = 127;
            BigEndian::write_u64(&mut header[2..], payload_len as u64);
            header_size = 10;
        };

        track!(self.header.start_encoding(header))?;
        self.header.set_consumable_bytes(header_size);
        self.payload_length = payload_len;
        Ok(())
    }
}
impl Encode for FrameEncoder {
    type Item = Frame;

    fn encode(&mut self, buf: &mut [u8], eos: Eos) -> bytecodec::Result<usize> {
        let mut offset = 0;
        if !self.header.is_suspended() {
            offset += track!(self.header.encode(buf, eos))?;
            if !self.header.is_suspended() {
                return Ok(offset);
            }
        }

        let size = cmp::min(
            buf.len() - offset,
            self.payload_length - self.payload_offset,
        );
        buf[offset..][..size].copy_from_slice(&self.payload[self.payload_offset..][..size]);
        self.payload_offset += size;
        if self.payload_offset == self.payload_length {
            self.payload_length = 0;
            self.payload_offset = 0;
            self.header = Default::default();
        }
        Ok(offset + size)
    }

    fn start_encoding(&mut self, item: Self::Item) -> bytecodec::Result<()> {
        track_assert!(self.is_idle(), bytecodec::ErrorKind::EncoderFull);
        match item {
            Frame::ConnectionClose { code, reason } => {
                track!(self.start_encoding_header(Opcode::ConnectionClose, 2 + reason.len()))?;
                self.payload_length = 2 + reason.len();
                track_assert!(
                    self.payload_length <= self.payload.len(),
                    bytecodec::ErrorKind::InvalidInput
                );
                BigEndian::write_u16(&mut self.payload, code);
                self.payload[2..][..reason.len()].copy_from_slice(&reason);
            }
            Frame::Pong { data } => {
                track!(self.start_encoding_header(Opcode::Pong, data.len()))?;
                self.payload_length = data.len();
                track_assert!(
                    self.payload_length <= self.payload.len(),
                    bytecodec::ErrorKind::InvalidInput
                );
                self.payload[..data.len()].copy_from_slice(&data);
            }
            Frame::Ping { .. } | Frame::Data => unreachable!(),
        }
        Ok(())
    }

    fn is_idle(&self) -> bool {
        self.header.is_idle() && self.payload_length == 0
    }

    fn requiring_bytes(&self) -> ByteCount {
        let n = self.header.consumable_bytes() + (self.payload_length - self.payload_offset) as u64;
        ByteCount::Finite(n)
    }
}
impl Default for FrameEncoder {
    fn default() -> Self {
        FrameEncoder {
            header: Default::default(),
            payload: vec![0; 4096],
            payload_length: 0,
            payload_offset: 0,
        }
    }
}

#[derive(Debug, Default)]
struct FrameHeaderDecoder {
    fixed_bytes: CopyableBytesDecoder<[u8; 2]>,
    extended_bytes: CopyableBytesDecoder<ExtendedHeaderBytes>,
    header: Option<FrameHeader>,
    completed: bool,
}
impl Decode for FrameHeaderDecoder {
    type Item = FrameHeader;

    fn decode(&mut self, buf: &[u8], eos: Eos) -> bytecodec::Result<usize> {
        if self.completed {
            return Ok(0);
        }

        let mut offset = 0;
        if self.header.is_none() {
            bytecodec_try_decode!(self.fixed_bytes, offset, buf, eos);
            let b = track!(self.fixed_bytes.finish_decoding())?;
            let header = track!(FrameHeader::from_bytes(b))?;

            self.extended_bytes.inner_mut().size = 0;
            if header.mask.is_some() {
                self.extended_bytes.inner_mut().size = 4;
            }
            match header.payload_len {
                126 => {
                    self.extended_bytes.inner_mut().size += 2;
                }
                127 => {
                    self.extended_bytes.inner_mut().size += 8;
                }
                _ => {}
            }
            self.header = Some(header);
        }

        bytecodec_try_decode!(self.extended_bytes, offset, buf, eos);
        let b = track!(self.extended_bytes.finish_decoding())?;
        let header = self.header.as_mut().expect("Never fails");
        let mut bytes = &b.bytes[..];
        match header.payload_len {
            126 => {
                header.payload_len = u64::from(BigEndian::read_u16(bytes));
                bytes = &bytes[2..];
            }
            127 => {
                header.payload_len = BigEndian::read_u64(bytes);
                bytes = &bytes[8..];
            }
            _ => {}
        }
        if header.mask.is_some() {
            header.mask = Some([bytes[0], bytes[1], bytes[2], bytes[3]]);
        }
        self.completed = true;
        Ok(offset)
    }

    fn finish_decoding(&mut self) -> bytecodec::Result<Self::Item> {
        track_assert!(self.completed, bytecodec::ErrorKind::IncompleteDecoding);
        let header =
            track_assert_some!(self.header.take(), bytecodec::ErrorKind::InconsistentState);
        self.completed = false;
        Ok(header)
    }

    fn requiring_bytes(&self) -> ByteCount {
        if self.completed {
            ByteCount::Finite(0)
        } else {
            self.fixed_bytes
                .requiring_bytes()
                .add_for_decoding(self.extended_bytes.requiring_bytes())
        }
    }
}

#[derive(Debug)]
struct FramePayloadDecoder {
    buf: Vec<u8>,
    buf_start: usize,
    buf_end: usize,
    payload_offset: u64,
    mask_offset: usize,
    header: Option<FrameHeader>,
}
impl Decode for FramePayloadDecoder {
    type Item = Frame;

    fn decode(&mut self, buf: &[u8], eos: Eos) -> bytecodec::Result<usize> {
        if let Some(ref header) = self.header {
            let size =
                cmp::min(header.payload_len - self.payload_offset, buf.len() as u64) as usize;
            let size = cmp::min(size, self.buf.len() - self.buf_end);
            self.buf[self.buf_end..][..size].copy_from_slice(&buf[..size]);
            self.buf_end += size;
            self.payload_offset += size as u64;
            if let Some(mask) = header.mask {
                let start = self.buf_end - size;
                for b in &mut self.buf[start..self.buf_end] {
                    *b ^= mask[self.mask_offset];
                    self.mask_offset = (self.mask_offset + 1) % 4;
                }
            }
            if self.payload_offset != header.payload_len {
                track_assert!(!eos.is_reached(), bytecodec::ErrorKind::UnexpectedEos);
            }
            Ok(size)
        } else {
            Ok(0)
        }
    }

    fn finish_decoding(&mut self) -> bytecodec::Result<Self::Item> {
        track_assert!(self.is_idle(), bytecodec::ErrorKind::IncompleteDecoding);
        let header =
            track_assert_some!(self.header.take(), bytecodec::ErrorKind::InconsistentState);
        let frame = match header.opcode {
            Opcode::ConnectionClose => {
                track_assert_eq!(self.buf_start, 0, bytecodec::ErrorKind::InconsistentState);
                track_assert!(self.buf_end >= 2, bytecodec::ErrorKind::InvalidInput);
                let code = BigEndian::read_u16(&self.buf);
                let reason = Vec::from(&self.buf[2..self.buf_end]);
                Frame::ConnectionClose { code, reason }
            }
            Opcode::Ping => {
                track_assert_eq!(self.buf_start, 0, bytecodec::ErrorKind::InconsistentState);
                let data = Vec::from(&self.buf[..self.buf_end]);
                Frame::Ping { data }
            }
            Opcode::Pong => {
                track_assert_eq!(self.buf_start, 0, bytecodec::ErrorKind::InconsistentState);
                let data = Vec::from(&self.buf[..self.buf_end]);
                Frame::Pong { data }
            }
            _ => {
                track_assert_eq!(
                    self.buf_start,
                    self.buf_end,
                    bytecodec::ErrorKind::InconsistentState
                );
                Frame::Data
            }
        };
        self.buf_start = 0;
        self.buf_end = 0;
        self.payload_offset = 0;
        self.mask_offset = 0;
        Ok(frame)
    }

    fn requiring_bytes(&self) -> ByteCount {
        if let Some(ref header) = self.header {
            ByteCount::Finite(header.payload_len - self.payload_offset)
        } else {
            ByteCount::Unknown
        }
    }

    fn is_idle(&self) -> bool {
        if let Some(ref header) = self.header {
            if header.payload_len == self.payload_offset {
                match header.opcode {
                    Opcode::ConnectionClose | Opcode::Ping | Opcode::Pong => true,
                    _ => self.buf_start == self.buf_end,
                }
            } else {
                false
            }
        } else {
            false
        }
    }
}
impl Default for FramePayloadDecoder {
    fn default() -> Self {
        FramePayloadDecoder {
            buf: vec![0; BUF_SIZE],
            buf_start: 0,
            buf_end: 0,
            payload_offset: 0,
            mask_offset: 0,
            header: None,
        }
    }
}

#[derive(Debug, Default)]
pub struct FrameDecoder {
    header: FrameHeaderDecoder,
    payload: FramePayloadDecoder,
}
impl FrameDecoder {
    pub fn write_decoded_data<W: Write>(&mut self, mut writer: W) -> Result<StreamState> {
        if self.is_data_empty() {
            return Ok(StreamState::Normal);
        }

        let buf = &self.payload.buf[self.payload.buf_start..self.payload.buf_end];
        match writer.write(buf) {
            Err(e) => {
                if e.kind() == io::ErrorKind::WouldBlock {
                    Ok(StreamState::WouldBlock)
                } else {
                    Err(track!(Error::from(e)))
                }
            }
            Ok(0) => Ok(StreamState::Eos),
            Ok(size) => {
                self.payload.buf_start += size;
                if self.payload.buf_start == self.payload.buf_end {
                    self.payload.buf_start = 0;
                    self.payload.buf_end = 0;
                }
                Ok(StreamState::Normal)
            }
        }
    }

    pub fn is_data_empty(&self) -> bool {
        self.payload.header.as_ref().map_or(true, |h| {
            h.opcode.is_control() || self.payload.buf_start == self.payload.buf_end
        })
    }
}
impl Decode for FrameDecoder {
    type Item = Frame;

    fn decode(&mut self, buf: &[u8], eos: Eos) -> bytecodec::Result<usize> {
        let mut offset = 0;
        if self.payload.header.is_none() {
            bytecodec_try_decode!(self.header, offset, buf, eos);
            let header = track!(self.header.finish_decoding())?;
            self.payload.header = Some(header);
        }
        bytecodec_try_decode!(self.payload, offset, buf, eos);
        Ok(offset)
    }

    fn finish_decoding(&mut self) -> bytecodec::Result<Self::Item> {
        track!(self.payload.finish_decoding())
    }

    fn requiring_bytes(&self) -> ByteCount {
        self.header
            .requiring_bytes()
            .add_for_decoding(self.payload.requiring_bytes())
    }

    fn is_idle(&self) -> bool {
        self.payload.is_idle()
    }
}

#[derive(Debug, Default, Clone, Copy)]
struct ExtendedHeaderBytes {
    bytes: [u8; 12],
    size: usize,
}
impl AsRef<[u8]> for ExtendedHeaderBytes {
    fn as_ref(&self) -> &[u8] {
        &self.bytes[..self.size]
    }
}
impl AsMut<[u8]> for ExtendedHeaderBytes {
    fn as_mut(&mut self) -> &mut [u8] {
        &mut self.bytes[..self.size]
    }
}
