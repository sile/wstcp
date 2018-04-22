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
use frame::{Frame, FrameDecoder, FrameEncoder};
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
    real_stream_rstate: StreamState,
    real_stream_wstate: StreamState,
    handshake: Handshake,
    closing: Closing,
    pending_pong: Option<Vec<u8>>,
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
            real_stream_rstate: StreamState::Normal,
            real_stream_wstate: StreamState::Normal,
            handshake: Handshake::new(),
            closing: Closing::NotYet,
            pending_pong: None,
            frame_decoder: FrameDecoder::default(),
            frame_encoder: FrameEncoder::default(),
        }
    }

    fn process_handshake(&mut self) -> bool {
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

    fn process_relay(&mut self) -> Result<()> {
        if let Err(e) = track!(self.handle_real_stream()) {
            warn!(self.logger, "{}", e);
            track!(self.starts_closing(1001, false))?;
        }
        if let Err(e) = track!(self.handle_ws_stream()) {
            warn!(self.logger, "{}", e);
            track!(self.starts_closing(1002, false))?;
        }
        Ok(())
    }

    fn handle_real_stream(&mut self) -> Result<()> {
        if let Some(mut stream) = self.real_stream.as_mut() {
            self.real_stream_rstate =
                track!(self.frame_encoder.start_encoding_if_needed(&mut stream))?;
            self.real_stream_wstate = track!(self.frame_decoder.write_decoded_data(&mut stream))?;
        }
        Ok(())
    }

    fn handle_ws_stream(&mut self) -> Result<()> {
        if self.frame_encoder.is_idle() {
            if let Some(data) = self.pending_pong.take() {
                debug!(self.logger, "Sends Ping frame: {:?}", data);
                track!(self.frame_encoder.start_encoding(Frame::Pong { data }))?;
            }
        }
        if self.frame_encoder.is_idle() {
            if let Closing::InProgress {
                code,
                ref mut server_closed,
                ..
            } = self.closing
            {
                if *server_closed == false {
                    let reason = Vec::new();
                    let frame = Frame::ConnectionClose { code, reason };
                    track!(self.frame_encoder.start_encoding(frame))?;
                    *server_closed = true;
                }
            }
        }

        track!(self.frame_encoder.encode_to_write_buf(&mut self.ws_wbuf))?;
        if self.frame_encoder.is_idle() && self.closing.is_both_closed() {
            self.closing = Closing::Closed;
        }

        if let Some(frame) = track!(self.frame_decoder.decode_from_read_buf(&mut self.ws_rbuf))? {
            debug!(self.logger, "Received frame: {:?}", frame);
            track!(self.handle_frame(frame))?;
        }
        Ok(())
    }

    fn handle_frame(&mut self, frame: Frame) -> Result<()> {
        match frame {
            Frame::ConnectionClose { code, reason } => {
                info!(
                    self.logger,
                    "Received Close frame: code={}, reason={:?}",
                    code,
                    String::from_utf8(reason)
                );
                match self.closing {
                    Closing::NotYet => {
                        track!(self.starts_closing(code, true))?;
                    }
                    Closing::InProgress {
                        server_closed: false,
                        ref mut client_closed,
                        ..
                    } => {
                        *client_closed = true;
                    }
                    Closing::InProgress {
                        server_closed: true,
                        ..
                    } => {
                        self.closing = Closing::Closed;
                    }
                    _ => track_panic!(ErrorKind::Other; self.closing),
                }
            }
            Frame::Ping { data } => {
                if self.closing.is_not_yet() {
                    self.pending_pong = Some(data);
                }
            }
            Frame::Pong { .. } | Frame::Data => {}
        }
        Ok(())
    }

    fn starts_closing(&mut self, code: u16, client_closed: bool) -> Result<()> {
        track_assert_eq!(self.closing, Closing::NotYet, ErrorKind::Other);
        self.real_stream = None;
        self.real_stream_rstate = StreamState::Eos;
        self.real_stream_wstate = StreamState::Eos;
        self.closing = Closing::InProgress {
            code,
            client_closed,
            server_closed: false,
        };
        Ok(())
    }

    fn is_ws_stream_eos(&self) -> bool {
        self.ws_rbuf.stream_state().is_eos() || self.ws_wbuf.stream_state().is_eos()
    }

    fn is_real_stream_eos(&self) -> bool {
        self.real_stream_rstate.is_eos() || self.real_stream_wstate.is_eos()
    }

    fn would_ws_stream_block(&self) -> bool {
        self.ws_rbuf.stream_state().would_block()
            && (self.ws_wbuf.is_empty() || self.ws_wbuf.stream_state().would_block())
    }

    fn would_real_stream_block(&self) -> bool {
        self.real_stream_rstate.would_block()
            && (self.frame_decoder.is_data_empty() || self.real_stream_wstate.would_block())
    }
}
impl Future for ProxyChannel {
    type Item = ();
    type Error = Error;

    fn poll(&mut self) -> Poll<Self::Item, Self::Error> {
        loop {
            // WebSocket TCP stream I/O
            track!(self.ws_rbuf.fill(&mut self.ws_stream))?;
            track!(self.ws_wbuf.flush(&mut self.ws_stream))?;
            if self.is_ws_stream_eos() {
                info!(self.logger, "TCP stream for WebSocket has been closed");
                return Ok(Async::Ready(()));
            }

            // WebSocket handshake
            if !self.process_handshake() {
                warn!(self.logger, "WebSocket handshake cannot be completed");
                return Ok(Async::Ready(()));
            }
            if !self.handshake.done() {
                if self.would_ws_stream_block() {
                    return Ok(Async::NotReady);
                }
                continue;
            }

            if self.closing == Closing::Closed {
                info!(self.logger, "WebSocket channel has been closed normally");
                return Ok(Async::Ready(()));
            }

            // Relay
            track!(self.process_relay())?;
            if self.is_real_stream_eos() && self.closing.is_not_yet() {
                info!(self.logger, "TCP stream for a real server has been closed");
                track!(self.starts_closing(1000, false))?;
            }
            if self.would_ws_stream_block() && self.would_real_stream_block() {
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

#[derive(Debug, PartialEq, Eq)]
enum Closing {
    NotYet,
    InProgress {
        code: u16,
        client_closed: bool,
        server_closed: bool,
    },
    Closed,
}
impl Closing {
    fn is_not_yet(&self) -> bool {
        Closing::NotYet == *self
    }

    fn is_both_closed(&self) -> bool {
        if let Closing::InProgress {
            client_closed: true,
            server_closed: true,
            ..
        } = *self
        {
            true
        } else {
            false
        }
    }
}
