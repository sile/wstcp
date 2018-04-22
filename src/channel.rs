use std::mem;
use std::net::SocketAddr;
use base64::encode;
use bytecodec::{Encode, EncodeExt};
use bytecodec::io::{IoDecodeExt, IoEncodeExt, ReadBuf, StreamState, WriteBuf};
use fibers::net::TcpStream;
use fibers::net::futures::Connect;
use futures::{Async, Future, Poll};
use httpcodec::{HeaderField, NoBodyDecoder, NoBodyEncoder, ReasonPhrase, Request, RequestDecoder,
                Response, ResponseEncoder, StatusCode};
use sha1::{Digest, Sha1};
use slog::Logger;

use {Error, ErrorKind, Result};
use frame::{FrameDecoder, FrameEncoder};

const GUID: &str = "258EAFA5-E914-47DA-95CA-C5AB0DC85B11";

#[derive(Debug)]
enum Handshake {
    RecvRequest(RequestDecoder<NoBodyDecoder>),
    SendResponse(ResponseEncoder<NoBodyEncoder>),
    ConnectToRealServer(Connect),
    Done,
}

#[derive(Debug)]
pub struct ProxyChannel {
    logger: Logger,
    stream: TcpStream,
    real_server: Option<TcpStream>,
    real_server_addr: SocketAddr,
    rbuf: ReadBuf<Vec<u8>>,
    wbuf: WriteBuf<Vec<u8>>,

    // TODO: delete
    real_wbuf: WriteBuf<Vec<u8>>,

    handshake: Handshake,
    frame_decoder: FrameDecoder,
    frame_encoder: FrameEncoder,
}
impl ProxyChannel {
    pub fn new(logger: Logger, stream: TcpStream, real_server_addr: SocketAddr) -> Self {
        let _ = unsafe { stream.with_inner(|s| s.set_nodelay(true)) };
        info!(logger, "New proxy channel is created");
        ProxyChannel {
            logger,
            stream,
            real_server: None,
            real_server_addr,
            rbuf: ReadBuf::new(vec![0; 1024]),
            wbuf: WriteBuf::new(vec![0; 1024]),
            real_wbuf: WriteBuf::new(vec![0; 1024]),
            handshake: Handshake::RecvRequest(RequestDecoder::default()),
            frame_decoder: FrameDecoder::default(),
            frame_encoder: FrameEncoder::default(),
        }
    }

    fn handle_handshake_request(&mut self, request: &Request<()>) -> Result<Response<()>> {
        let mut key = None;
        for field in request.header().fields() {
            let name = field.name();
            let value = field.value();
            if name.eq_ignore_ascii_case("upgrade") {
                track_assert_eq!(value, "websocket", ErrorKind::InvalidInput);
            } else if name.eq_ignore_ascii_case("connection") {
                track_assert_eq!(value, "Upgrade", ErrorKind::InvalidInput);
            } else if name.eq_ignore_ascii_case("sec-websocket-key") {
                key = Some(value.to_owned());
            } else if name.eq_ignore_ascii_case("sec-websocket-version") {
                track_assert_eq!(value, "13", ErrorKind::InvalidInput);
            } else if name.eq_ignore_ascii_case("sec-websocket-protocol") {
                // TODO: handle
            }
        }

        let key = track_assert_some!(key, ErrorKind::InvalidInput);
        let hash = accept_hash(&key);

        let mut response = Response::new(
            request.http_version(),
            StatusCode::new(101)?,
            ReasonPhrase::new("Switching Protocols")?,
            (),
        );
        response
            .header_mut()
            .add_field(HeaderField::new("Upgrade", "websocket")?)
            .add_field(HeaderField::new("Connection", "Upgrade")?)
            .add_field(HeaderField::new("Sec-WebSocket-Accept", &hash)?);
        Ok(response)
    }

    fn poll_handshake(&mut self) -> Result<()> {
        loop {
            match mem::replace(&mut self.handshake, Handshake::Done) {
                Handshake::RecvRequest(mut d) => {
                    if let Some(request) = track!(d.decode_from_read_buf(&mut self.rbuf))? {
                        debug!(self.logger, "Method: {}", request.method()); // TODO: GET
                        debug!(self.logger, "Target: {}", request.request_target());
                        debug!(self.logger, "Version: {}", request.http_version()); // TODO: 1.1
                        debug!(
                            self.logger,
                            "Header: {:?}",
                            request.header().fields().collect::<Vec<_>>()
                        );

                        // TODO: return error response when fails
                        let response = track!(self.handle_handshake_request(&request))?;
                        let encoder = track!(ResponseEncoder::with_item(response))?;
                        self.handshake = Handshake::SendResponse(encoder);
                    } else {
                        self.handshake = Handshake::RecvRequest(d);
                        break;
                    }
                }
                Handshake::SendResponse(mut e) => {
                    // TODO: ConnectToRealServer と順番を逆にして、失敗ならエラー応答を返す
                    track!(e.encode_to_write_buf(&mut self.wbuf))?;
                    if e.is_idle() {
                        debug!(self.logger, "Write handshake response");
                        self.handshake = Handshake::ConnectToRealServer(TcpStream::connect(
                            self.real_server_addr,
                        ));
                    } else {
                        self.handshake = Handshake::SendResponse(e);
                    }
                    break;
                }
                Handshake::ConnectToRealServer(mut f) => {
                    let item = track!(f.poll().map_err(Error::from), "Connecting to real server")?;
                    if let Async::Ready(stream) = item {
                        debug!(self.logger, "Connected to the real server");
                        let _ = unsafe { stream.with_inner(|s| s.set_nodelay(true)) };
                        self.handshake = Handshake::Done;
                        self.real_server = Some(stream);
                    } else {
                        self.handshake = Handshake::ConnectToRealServer(f);
                    }
                }
                Handshake::Done => {
                    break;
                }
            }
        }
        Ok(())
    }
}
impl Future for ProxyChannel {
    type Item = ();
    type Error = Error;

    fn poll(&mut self) -> Poll<Self::Item, Self::Error> {
        let mut real_read_stream_state = StreamState::Normal;
        loop {
            track!(self.rbuf.fill(&mut self.stream))?;
            track!(self.wbuf.flush(&mut self.stream))?;
            track!(self.poll_handshake())?;
            if let Some(mut real) = self.real_server.as_mut() {
                real_read_stream_state =
                    track!(self.frame_encoder.start_encoding_if_needed(&mut real))?;
                track!(self.frame_encoder.encode_to_write_buf(&mut self.wbuf))?;

                track!(self.real_wbuf.flush(&mut real))?;
                if let Some(item) = track!(self.frame_decoder.decode_from_read_buf(&mut self.rbuf))?
                {
                    info!(self.logger, "item={:?}", item);
                }
                track!(
                    self.frame_decoder
                        .write_decoded_payload(&mut self.real_wbuf)
                )?;
            }
            if self.rbuf.stream_state().is_eos() || self.wbuf.stream_state().is_eos() {
                // TODO: handle real_read_stream_state.is_eos()
                return Ok(Async::Ready(()));
            }

            let read_would_block = self.rbuf.stream_state().would_block();
            let write_would_block = self.wbuf.is_empty() || self.wbuf.stream_state().would_block();
            let real_read_would_block = real_read_stream_state.would_block();
            let real_write_would_block =
                self.real_wbuf.is_empty() || self.real_wbuf.stream_state().would_block();
            if read_would_block && write_would_block && real_read_would_block
                && real_write_would_block
            {
                return Ok(Async::NotReady);
            }
        }
    }
}

fn accept_hash(key: &str) -> String {
    let mut sh = Sha1::default();
    sh.input(format!("{}{}", key, GUID).as_bytes());
    let output = sh.result();
    encode(&output)
}

#[cfg(test)]
mod test {
    use super::*;

    #[test]
    fn it_works() {
        let hash = accept_hash("dGhlIHNhbXBsZSBub25jZQ==");
        assert_eq!(hash, "s3pPLMBiTxaQ9kYGzzhZRbK+xOo=");
    }
}
