use bytecodec::{self, ByteCount, Decode, Eos};
use bytecodec::bytes::CopyableBytesDecoder;
use byteorder::{BigEndian, ByteOrder};

use opcode::Opcode;

const FIN_FLAG: u8 = 0b1000_0000;
const MASK_FLAG: u8 = 0b1000_0000;

#[derive(Debug)]
pub enum ControlFrame {
    ConnectionClose { code: u16, reason: Vec<u8> },
    Ping { data: Vec<u8> },
    Pong { data: Vec<u8> },
}

#[derive(Debug)]
pub struct FrameHeader {
    fin_flag: bool,
    opcode: Opcode,
    mask: Option<u32>,
    payload_len: u64,
}

#[derive(Debug, Default)]
pub struct FrameHeaderDecoder {
    fixed_bytes: CopyableBytesDecoder<[u8; 4]>,
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
                    header.mask = Some(0); // dummy
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
                header.mask = Some(BigEndian::read_u32(bytes));
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

#[derive(Debug, Default)]
pub struct FrameDecoder {
    header: FrameHeaderDecoder,
}
impl Decode for FrameDecoder {
    // TODO:
    type Item = FrameHeader;

    fn decode(&mut self, buf: &[u8], eos: Eos) -> bytecodec::Result<(usize, Option<Self::Item>)> {
        self.header.decode(buf, eos)
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
