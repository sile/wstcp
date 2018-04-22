use std::cmp;
use std::io::{self, Read, Write};
use bytecodec::{self, ByteCount, Decode, Encode, Eos};
use bytecodec::bytes::{BytesEncoder, CopyableBytesDecoder};
use bytecodec::combinator::Slice;
use bytecodec::io::StreamState;
use byteorder::{BigEndian, ByteOrder};

use {Error, Result};
use opcode::Opcode;

const FIN_FLAG: u8 = 0b1000_0000;
const MASK_FLAG: u8 = 0b1000_0000;

#[derive(Debug)]
pub enum Frame {
    ConnectionClose { code: u16, reason: Vec<u8> },
    Ping { data: Vec<u8> },
    Pong { data: Vec<u8> },
    Data,
}

#[derive(Debug, Clone)]
pub struct FrameHeader {
    fin_flag: bool,
    opcode: Opcode,
    mask: Option<[u8; 4]>,
    payload_len: u64,
}

#[derive(Debug)]
pub struct FrameEncoder {
    header: Slice<BytesEncoder<[u8; 2 + 8]>>,
    payload: Vec<u8>,
    payload_offset: usize,
    payload_length: usize,
}
impl FrameEncoder {
    pub fn start_encoding_if_needed<R: Read>(&mut self, mut reader: R) -> Result<StreamState> {
        if self.payload_length != 0 {
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
                let header_size;
                let mut header = [0; 2 + 8];
                header[0] = FIN_FLAG | (Opcode::BinaryFrame as u8);
                if size < 126 {
                    header[1] = size as u8;
                    header_size = 2;
                } else if size < 0x10000 {
                    header[1] = 126;
                    BigEndian::write_u16(&mut header[2..], size as u16);
                    header_size = 4;
                } else {
                    header[1] = 127;
                    BigEndian::write_u64(&mut header[2..], size as u64);
                    header_size = 10;
                };

                track!(self.header.start_encoding(header))?;
                self.header.set_consumable_bytes(header_size);
                self.payload_length = size;
            }
        }
        Ok(StreamState::Normal)
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
        (&mut buf[offset..][..size])
            .copy_from_slice(&mut self.payload[self.payload_offset..][..size]);
        self.payload_offset += size;
        if self.payload_offset == self.payload_length {
            self.payload_length = 0;
            self.payload_offset = 0;

            // TODO:
            self.header.set_consumable_bytes(10);
            track!(self.header.encode(&mut [0; 10][..], Eos::new(false)))?;
        }
        Ok(offset + size)
    }

    fn start_encoding(&mut self, _item: Self::Item) -> bytecodec::Result<()> {
        unimplemented!()
    }

    fn is_idle(&self) -> bool {
        self.payload_length == 0
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
pub struct FrameHeaderDecoder {
    fixed_bytes: CopyableBytesDecoder<[u8; 2]>,
    extended_bytes: CopyableBytesDecoder<ExtendedHeaderBytes>,
    header: Option<FrameHeader>,
}
impl Decode for FrameHeaderDecoder {
    type Item = FrameHeader;

    fn decode(&mut self, buf: &[u8], eos: Eos) -> bytecodec::Result<(usize, Option<Self::Item>)> {
        let mut offset = 0;
        if self.header.is_none() {
            let (size, item) = track!(self.fixed_bytes.decode(buf, eos))?;
            offset += size;
            if let Some(b) = item {
                // TODO: check rsv flags
                let mut header = FrameHeader {
                    fin_flag: (b[0] & FIN_FLAG) != 0,
                    opcode: track!(Opcode::from_u8(b[0] & 0b1111))?,
                    mask: None,
                    payload_len: u64::from(b[1] & 0b0111_1111),
                };

                self.extended_bytes.inner_mut().size = 0;
                let mask_flag = (b[1] & MASK_FLAG) != 0;
                if mask_flag {
                    self.extended_bytes.inner_mut().size = 4;
                    header.mask = Some([0; 4]); // dummy
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
            } else {
                return Ok((offset, None));
            }
        }

        let (size, item) = track!(self.extended_bytes.decode(&buf[offset..], eos))?;
        offset += size;
        if let Some(b) = item {
            let mut header = track_assert_some!(self.header.take(), bytecodec::ErrorKind::Other);
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
            Ok((offset, Some(header)))
        } else {
            Ok((offset, None))
        }
    }

    fn has_terminated(&self) -> bool {
        false
    }

    fn requiring_bytes(&self) -> ByteCount {
        self.fixed_bytes
            .requiring_bytes()
            .add_for_decoding(self.extended_bytes.requiring_bytes())
    }
}

#[derive(Debug)]
pub struct FramePayloadDecoder {
    buf: Vec<u8>,
    buf_start: usize,
    buf_end: usize,
    payload_offset: u64,
    mask_offset: usize,
    header: Option<FrameHeader>,
}
impl Decode for FramePayloadDecoder {
    type Item = Frame;

    fn decode(&mut self, buf: &[u8], eos: Eos) -> bytecodec::Result<(usize, Option<Self::Item>)> {
        let header = track_assert_some!(self.header.clone(), bytecodec::ErrorKind::Other);
        let size = cmp::min(header.payload_len - self.payload_offset, buf.len() as u64) as usize;
        let size = cmp::min(size, self.buf.len() - self.buf_end);
        (&mut self.buf[self.buf_end..][..size]).copy_from_slice(&buf[..size]);
        self.buf_end += size;
        self.payload_offset += size as u64;
        if let Some(mask) = header.mask {
            let start = self.buf_end - size;
            for b in &mut self.buf[start..self.buf_end] {
                *b ^= mask[self.mask_offset];
                self.mask_offset = (self.mask_offset + 1) % 4;
            }
        }
        if self.payload_offset == header.payload_len {
            let frame = match header.opcode {
                Opcode::ConnectionClose => {
                    track_assert_eq!(self.buf_start, 0, bytecodec::ErrorKind::Other);
                    track_assert!(self.buf_end >= 2, bytecodec::ErrorKind::InvalidInput);
                    let code = BigEndian::read_u16(&self.buf);
                    let reason = Vec::from(&self.buf[2..]);
                    Frame::ConnectionClose { code, reason }
                }
                Opcode::Ping => {
                    track_assert_eq!(self.buf_start, 0, bytecodec::ErrorKind::Other);
                    let data = Vec::from(&self.buf[..]);
                    Frame::Ping { data }
                }
                Opcode::Pong => {
                    track_assert_eq!(self.buf_start, 0, bytecodec::ErrorKind::Other);
                    let data = Vec::from(&self.buf[..]);
                    Frame::Pong { data }
                }
                _ => {
                    if self.buf_start == self.buf_end {
                        Frame::Data
                    } else {
                        return Ok((size, None));
                    }
                }
            };
            self.buf_start = 0;
            self.buf_end = 0;
            self.payload_offset = 0;
            self.mask_offset = 0;
            self.header = None;
            Ok((size, Some(frame)))
        } else {
            track_assert!(!eos.is_reached(), bytecodec::ErrorKind::UnexpectedEos);
            Ok((size, None))
        }
    }

    fn has_terminated(&self) -> bool {
        false
    }

    fn requiring_bytes(&self) -> ByteCount {
        if let Some(ref header) = self.header {
            ByteCount::Finite(header.payload_len - self.payload_offset)
        } else {
            ByteCount::Unknown
        }
    }
}
impl Default for FramePayloadDecoder {
    fn default() -> Self {
        FramePayloadDecoder {
            buf: vec![0; 4096],
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

    fn decode(&mut self, buf: &[u8], eos: Eos) -> bytecodec::Result<(usize, Option<Self::Item>)> {
        let mut offset = 0;
        if self.payload.header.is_none() {
            let (size, item) = track!(self.header.decode(buf, eos))?;
            offset += size;
            if let Some(header) = item {
                self.payload.header = Some(header);
            } else {
                return Ok((offset, None));
            }
        }

        let (size, item) = track!(self.payload.decode(&buf[offset..], eos))?;
        offset += size;
        Ok((offset, item))
    }

    fn has_terminated(&self) -> bool {
        self.header.has_terminated()
    }

    fn requiring_bytes(&self) -> ByteCount {
        self.header.requiring_bytes()
    }
}

#[derive(Debug, Default, Clone, Copy)]
pub struct ExtendedHeaderBytes {
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
