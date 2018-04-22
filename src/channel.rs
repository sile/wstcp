use std::mem;
use std::net::SocketAddr;
use bytecodec::{Encode, EncodeExt};
use bytecodec::io::{IoDecodeExt, IoEncodeExt, ReadBuf, StreamState, WriteBuf};
use fibers::net::TcpStream;
use fibers::net::futures::Connect;
use futures::{Async, Future, Poll};
use httpcodec::{HeaderField, HttpVersion, NoBodyDecoder, NoBodyEncoder, ReasonPhrase, Request,
                RequestDecoder, Response, ResponseEncoder, StatusCode};
use slog::Logger;

use {Error, ErrorKind, Result};
use frame::{FrameDecoder, FrameEncoder};
use util::{self, WebSocketKey};

const BUF_SIZE: usize = 4096;

#[derive(Debug)]
pub struct ProxyChannel {
    logger: Logger,
    ws_stream: TcpStream,
    ws_rbuf: ReadBuf<Vec<u8>>,
    ws_wbuf: WriteBuf<Vec<u8>>,
    real_server_addr: SocketAddr,
    real_stream: Option<TcpStream>,

    // TODO: delete
    real_wbuf: WriteBuf<Vec<u8>>,

    handshake: Handshake,
    frame_decoder: FrameDecoder,
    frame_encoder: FrameEncoder,
}
impl ProxyChannel {
    pub fn new(logger: Logger, ws_stream: TcpStream, real_server_addr: SocketAddr) -> Self {
        let _ = unsafe { ws_stream.with_inner(|s| s.set_nodelay(true)) };
        info!(logger, "New proxy channel is created");
        ProxyChannel {
            logger,
            ws_stream,
            ws_rbuf: ReadBuf::new(vec![0; BUF_SIZE]),
            ws_wbuf: WriteBuf::new(vec![0; BUF_SIZE]),
            real_server_addr,
            real_stream: None,
            real_wbuf: WriteBuf::new(vec![0; BUF_SIZE]),
            handshake: Handshake::new(),
            frame_decoder: FrameDecoder::default(),
            frame_encoder: FrameEncoder::default(),
        }
    }

    fn poll_handshake(&mut self) -> bool {
        loop {
            match mem::replace(&mut self.handshake, Handshake::Done) {
                Handshake::RecvRequest(mut decoder) => match track!(
                    decoder.decode_from_read_buf(&mut self.ws_rbuf)
                ) {
                    Err(e) => {
                        warn!(self.logger, "Malformed HTTP request: {}", e);
                        self.handshake = Handshake::response_bad_request();
                    }
                    Ok(None) => {
                        self.handshake = Handshake::RecvRequest(decoder);
                        break;
                    }
                    Ok(Some(request)) => {
                        debug!(self.logger, "Received a WebSocket handshake request");
                        debug!(self.logger, "Method: {}", request.method());
                        debug!(self.logger, "Target: {}", request.request_target());
                        debug!(self.logger, "Version: {}", request.http_version());
                        debug!(self.logger, "Header: {}", request.header());

                        match track!(self.handle_handshake_request(&request)) {
                            Err(e) => {
                                warn!(self.logger, "Invalid WebSocket handshake request: {}", e);
                                self.handshake = Handshake::response_bad_request();
                            }
                            Ok(key) => {
                                debug!(self.logger, "Tries to connect the real server");
                                let future = TcpStream::connect(self.real_server_addr);
                                self.handshake = Handshake::ConnectToRealServer(future, key);
                            }
                        }
                    }
                },
                Handshake::ConnectToRealServer(mut f, key) => {
                    match track!(f.poll().map_err(Error::from), "Connecting to real server") {
                        Err(e) => {
                            warn!(self.logger, "Cannot connect to the real server: {}", e);
                            self.handshake = Handshake::response_unavailable();
                        }
                        Ok(Async::NotReady) => {
                            self.handshake = Handshake::ConnectToRealServer(f, key);
                            break;
                        }
                        Ok(Async::Ready(stream)) => {
                            debug!(self.logger, "Connected to the real server");
                            let _ = unsafe { stream.with_inner(|s| s.set_nodelay(true)) };
                            if let Ok(addr) = stream.local_addr() {
                                self.logger = self.logger.new(o!("relay_addr" => addr.to_string()));
                            }
                            self.handshake = Handshake::response_accepted(&key);
                            self.real_stream = Some(stream);
                        }
                    }
                }
                Handshake::SendResponse(mut encoder, succeeded) => {
                    if let Err(e) = track!(encoder.encode_to_write_buf(&mut self.ws_wbuf)) {
                        warn!(self.logger, "Cannot write a handshake response: {}", e);
                        return false;
                    }
                    if encoder.is_idle() {
                        debug!(self.logger, "Handshake response has been written");
                        if succeeded {
                            info!(self.logger, "WebSocket handshake succeeded");
                            self.handshake = Handshake::Done;
                        } else {
                            return false;
                        }
                    } else {
                        self.handshake = Handshake::SendResponse(encoder, succeeded);
                    }
                    break;
                }
                Handshake::Done => {
                    break;
                }
            }
        }
        true
    }

    fn handle_handshake_request(&mut self, request: &Request<()>) -> Result<WebSocketKey> {
        track_assert_eq!(request.method().as_str(), "GET", ErrorKind::InvalidInput);
        track_assert_eq!(
            request.http_version(),
            HttpVersion::V1_1,
            ErrorKind::InvalidInput
        );

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
            }
        }

        let key = track_assert_some!(key, ErrorKind::InvalidInput);
        Ok(WebSocketKey(key))
    }

    fn is_ws_stream_eos(&self) -> bool {
        self.ws_rbuf.stream_state().is_eos() || self.ws_wbuf.stream_state().is_eos()
    }

    fn would_ws_stream_block(&self) -> bool {
        let read_would_block = self.ws_rbuf.stream_state().would_block();
        let write_would_block =
            self.ws_wbuf.is_empty() || self.ws_wbuf.stream_state().would_block();
        read_would_block && write_would_block
    }
}
impl Future for ProxyChannel {
    type Item = ();
    type Error = Error;

    fn poll(&mut self) -> Poll<Self::Item, Self::Error> {
        let mut real_read_stream_state = StreamState::Normal;
        loop {
            // WebSocket TCP stream I/O
            track!(self.ws_rbuf.fill(&mut self.ws_stream))?;
            track!(self.ws_wbuf.flush(&mut self.ws_stream))?;
            if self.is_ws_stream_eos() {
                return Ok(Async::Ready(()));
            }

            // WebSocket handshake
            if !self.poll_handshake() {
                warn!(self.logger, "The WebSocket handshake cannot be completed");
                return Ok(Async::Ready(()));
            }
            if !self.handshake.done() {
                if self.would_ws_stream_block() {
                    return Ok(Async::NotReady);
                }
                continue;
            }

            if let Some(mut real) = self.real_stream.as_mut() {
                real_read_stream_state =
                    track!(self.frame_encoder.start_encoding_if_needed(&mut real))?;
                track!(self.frame_encoder.encode_to_write_buf(&mut self.ws_wbuf))?;

                track!(self.real_wbuf.flush(&mut real))?;
                if let Some(item) =
                    track!(self.frame_decoder.decode_from_read_buf(&mut self.ws_rbuf))?
                {
                    info!(self.logger, "item={:?}", item);
                }
                track!(
                    self.frame_decoder
                        .write_decoded_payload(&mut self.real_wbuf)
                )?;
            }
            // if self.ws_rbuf.stream_state().is_eos() || self.ws_wbuf.stream_state().is_eos() {
            //     // TODO: handle real_read_stream_state.is_eos()
            //     return Ok(Async::Ready(()));
            // }

            let read_would_block = self.ws_rbuf.stream_state().would_block();
            let write_would_block =
                self.ws_wbuf.is_empty() || self.ws_wbuf.stream_state().would_block();
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

#[derive(Debug)]
enum Handshake {
    RecvRequest(RequestDecoder<NoBodyDecoder>),
    ConnectToRealServer(Connect, WebSocketKey),
    SendResponse(ResponseEncoder<NoBodyEncoder>, bool),
    Done,
}
impl Handshake {
    fn new() -> Self {
        Handshake::RecvRequest(RequestDecoder::default())
    }

    fn done(&self) -> bool {
        if let Handshake::Done = *self {
            true
        } else {
            false
        }
    }

    fn response_accepted(key: &WebSocketKey) -> Self {
        let hash = util::calc_accept_hash(&key);

        unsafe {
            let mut response = Response::new(
                HttpVersion::V1_1,
                StatusCode::new_unchecked(101),
                ReasonPhrase::new_unchecked("Switching Protocols"),
                (),
            );
            response
                .header_mut()
                .add_field(HeaderField::new_unchecked("Upgrade", "websocket"))
                .add_field(HeaderField::new_unchecked("Connection", "Upgrade"))
                .add_field(HeaderField::new_unchecked("Sec-WebSocket-Accept", &hash));

            let encoder = ResponseEncoder::with_item(response).expect("Never fails");
            Handshake::SendResponse(encoder, true)
        }
    }

    fn response_bad_request() -> Self {
        unsafe {
            let mut response = Response::new(
                HttpVersion::V1_1,
                StatusCode::new_unchecked(400),
                ReasonPhrase::new_unchecked("Bad Request"),
                (),
            );
            response
                .header_mut()
                .add_field(HeaderField::new_unchecked("Content-Length", "0"));
            let encoder = ResponseEncoder::with_item(response).expect("Never fails");
            Handshake::SendResponse(encoder, false)
        }
    }

    fn response_unavailable() -> Self {
        unsafe {
            let mut response = Response::new(
                HttpVersion::V1_1,
                StatusCode::new_unchecked(503),
                ReasonPhrase::new_unchecked("Service Unavailable"),
                (),
            );
            response
                .header_mut()
                .add_field(HeaderField::new_unchecked("Content-Length", "0"));
            let encoder = ResponseEncoder::with_item(response).expect("Never fails");
            Handshake::SendResponse(encoder, false)
        }
    }
}
