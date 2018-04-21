use std::mem;
use std::net::SocketAddr;
use base64::encode;
use bytecodec::{Encode, EncodeExt};
use bytecodec::io::{IoDecodeExt, IoEncodeExt, ReadBuf, WriteBuf};
use fibers::Spawn;
use fibers::net::{TcpListener, TcpStream};
use fibers::net::futures::{Connected, TcpListenerBind};
use fibers::net::streams::Incoming;
use futures::{Async, Future, Poll, Stream};
use httpcodec::{HeaderField, NoBodyDecoder, NoBodyEncoder, ReasonPhrase, Request, RequestDecoder,
                Response, ResponseEncoder, StatusCode};
use sha1::{Digest, Sha1};
use slog::Logger;

use {Error, ErrorKind, Result};
use frame::FrameDecoder;

const GUID: &str = "258EAFA5-E914-47DA-95CA-C5AB0DC85B11";

#[derive(Debug)]
pub struct ProxyServer<S> {
    logger: Logger,
    spawner: S,
    ws_server_addr: SocketAddr,
    tcp_server_addr: SocketAddr,
    listener: Listener,
    connected: Vec<(SocketAddr, Connected)>,
}
impl<S> ProxyServer<S> {
    pub fn new(
        logger: Logger,
        spawner: S,
        ws_server_addr: SocketAddr,
        tcp_server_addr: SocketAddr,
    ) -> Self {
        let logger = logger.new(
            o!("ws_addr" => ws_server_addr.to_string(), "tcp_addr" => tcp_server_addr.to_string()),
        );
        info!(logger, "Starts WebSocket server");
        let listener = Listener::Binding(TcpListener::bind(ws_server_addr));
        ProxyServer {
            logger,
            spawner,
            ws_server_addr,
            tcp_server_addr,
            listener,
            connected: Vec::new(),
        }
    }
}
impl<S: Spawn> Future for ProxyServer<S> {
    type Item = ();
    type Error = Error;

    fn poll(&mut self) -> Poll<Self::Item, Self::Error> {
        loop {
            match track!(self.listener.poll())? {
                Async::NotReady => {
                    break;
                }
                Async::Ready(None) => {
                    warn!(
                        self.logger,
                        "TCP socket for WebSocket server has been closed"
                    );
                    return Ok(Async::Ready(()));
                }
                Async::Ready(Some((connected, addr))) => {
                    info!(self.logger, "New client arrived: {}", addr);
                    self.connected.push((addr, connected));
                }
            }
        }

        let mut i = 0;
        while i < self.connected.len() {
            match track!(self.connected[i].1.poll().map_err(Error::from)) {
                Err(e) => {
                    warn!(
                        self.logger,
                        "Cannot initialize client socket {}: {}", self.connected[i].0, e
                    );
                    self.connected.swap_remove(i);
                }
                Ok(Async::NotReady) => {
                    i += 1;
                }
                Ok(Async::Ready(stream)) => {
                    let (addr, _) = self.connected.swap_remove(i);
                    let logger = self.logger.new(o!("client_addr" => addr.to_string()));
                    let channel = ProxyChannel::new(logger.clone(), stream);
                    self.spawner.spawn(channel.then(move |result| match result {
                        Err(e) => {
                            warn!(logger, "Proxy channel aborted: {}", e);
                            Ok(())
                        }
                        Ok(()) => {
                            info!(logger, "Proxy channel termnated");
                            Ok(())
                        }
                    }));
                }
            }
        }
        Ok(Async::NotReady)
    }
}

#[derive(Debug)]
enum Listener {
    Binding(TcpListenerBind),
    Listening(Incoming),
}
impl Stream for Listener {
    type Item = (Connected, SocketAddr);
    type Error = Error;

    fn poll(&mut self) -> Poll<Option<Self::Item>, Self::Error> {
        loop {
            let next = match *self {
                Listener::Binding(ref mut f) => {
                    if let Async::Ready(listener) = track!(f.poll().map_err(Error::from))? {
                        Listener::Listening(listener.incoming())
                    } else {
                        return Ok(Async::NotReady);
                    }
                }
                Listener::Listening(ref mut s) => {
                    return track!(s.poll().map_err(Error::from));
                }
            };
            *self = next;
        }
    }
}

#[derive(Debug)]
enum Handshake {
    RecvRequest(RequestDecoder<NoBodyDecoder>),
    SendResponse(ResponseEncoder<NoBodyEncoder>),
    Done,
}
impl Handshake {
    fn has_done(&self) -> bool {
        if let Handshake::Done = *self {
            true
        } else {
            false
        }
    }
}

#[derive(Debug)]
struct ProxyChannel {
    logger: Logger,
    stream: TcpStream,
    rbuf: ReadBuf<Vec<u8>>,
    wbuf: WriteBuf<Vec<u8>>,
    handshake: Handshake,
    frame_decoder: FrameDecoder,
}
impl ProxyChannel {
    fn new(logger: Logger, stream: TcpStream) -> Self {
        let _ = unsafe { stream.with_inner(|s| s.set_nodelay(true)) };
        ProxyChannel {
            logger,
            stream,
            rbuf: ReadBuf::new(vec![0; 1024]),
            wbuf: WriteBuf::new(vec![0; 1024]),
            handshake: Handshake::RecvRequest(RequestDecoder::default()),
            frame_decoder: FrameDecoder::default(),
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
                        debug!(self.logger, "Method: {}", request.method());
                        debug!(self.logger, "Target: {}", request.request_target());
                        debug!(self.logger, "Version: {}", request.http_version());
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
                    track!(e.encode_to_write_buf(&mut self.wbuf))?;
                    if e.is_idle() {
                        debug!(self.logger, "Write handshake response");
                        self.handshake = Handshake::Done;
                    } else {
                        self.handshake = Handshake::SendResponse(e);
                    }
                    break;
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
        loop {
            track!(self.rbuf.fill(&mut self.stream))?;
            track!(self.wbuf.flush(&mut self.stream))?;
            track!(self.poll_handshake())?;
            if self.handshake.has_done() {
                if let Some(item) = track!(self.frame_decoder.decode_from_read_buf(&mut self.rbuf))?
                {
                    info!(self.logger, "item={:?}", item);
                }
            }
            if self.rbuf.stream_state().is_eos() || self.wbuf.stream_state().is_eos() {
                return Ok(Async::Ready(()));
            }

            let read_would_block = self.rbuf.stream_state().would_block();
            let write_would_block = self.wbuf.is_empty() || self.wbuf.stream_state().would_block();
            if read_would_block && write_would_block {
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
