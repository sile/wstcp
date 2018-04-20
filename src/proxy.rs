use std::net::SocketAddr;
use bytecodec::io::{IoDecodeExt, ReadBuf};
use fibers::Spawn;
use fibers::net::{TcpListener, TcpStream};
use fibers::net::futures::{Connected, TcpListenerBind};
use fibers::net::streams::Incoming;
use futures::{Async, Future, Poll, Stream};
use httpcodec::{HeadBodyDecoder, Request, RequestDecoder};
use slog::Logger;

use {Error, ErrorKind, Result};

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
                    let channel = ProxyChannel::new(stream);
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
struct ProxyChannel {
    stream: TcpStream,
    rbuf: ReadBuf<Vec<u8>>,
    request: RequestDecoder<HeadBodyDecoder>,
}
impl ProxyChannel {
    fn new(stream: TcpStream) -> Self {
        ProxyChannel {
            stream,
            rbuf: ReadBuf::new(vec![0; 1024]),
            request: RequestDecoder::default(),
        }
    }

    fn handle_handshake(&mut self, request: &Request<()>) -> Result<()> {
        // TODO: error response
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
        Ok(())
    }
}
impl Future for ProxyChannel {
    type Item = ();
    type Error = Error;

    fn poll(&mut self) -> Poll<Self::Item, Self::Error> {
        loop {
            // receive
            track!(self.rbuf.fill(&mut self.stream))?;
            if let Some(request) = track!(self.request.decode_from_read_buf(&mut self.rbuf))? {
                println!("{:?}", request);
                println!("# {:?}", request.method());
                println!("# {:?}", request.request_target());
                println!("# {:?}", request.http_version());
                println!("# {:?}", request.header().fields().collect::<Vec<_>>());
                track!(self.handle_handshake(&request))?;
            }
            if self.rbuf.stream_state().is_eos() {
                return Ok(Async::Ready(()));
            }
            if self.rbuf.stream_state().would_block() {
                return Ok(Async::NotReady);
            }
        }
    }
}
