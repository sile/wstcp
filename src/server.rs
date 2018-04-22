use std::net::SocketAddr;
use fibers::Spawn;
use fibers::net::TcpListener;
use fibers::net::futures::{Connected, TcpListenerBind};
use fibers::net::streams::Incoming;
use futures::{Async, Future, Poll, Stream};
use slog::Logger;

use Error;
use channel::ProxyChannel;

/// WebSocket to TCP proxy server.
#[derive(Debug)]
pub struct ProxyServer<S> {
    logger: Logger,
    spawner: S,
    proxy_addr: SocketAddr,
    real_server_addr: SocketAddr,
    listener: Listener,
    connected: Vec<(SocketAddr, Connected)>,
}
impl<S> ProxyServer<S> {
    /// Makes a new `ProxyServer` instance.
    pub fn new(
        logger: Logger,
        spawner: S,
        proxy_addr: SocketAddr,
        real_server_addr: SocketAddr,
    ) -> Self {
        let logger = logger.new(
            o!("proxy_addr" => proxy_addr.to_string(), "server_addr" => real_server_addr.to_string()),
        );
        info!(logger, "Starts a WebSocket proxy server");
        let listener = Listener::Binding(TcpListener::bind(proxy_addr));
        ProxyServer {
            logger,
            spawner,
            proxy_addr,
            real_server_addr,
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
                        "TCP socket for the WebSocket proxy server has been closed"
                    );
                    return Ok(Async::Ready(()));
                }
                Async::Ready(Some((connected, addr))) => {
                    debug!(self.logger, "New client arrived: {}", addr);
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
                    let channel = ProxyChannel::new(logger.clone(), stream, self.real_server_addr);
                    self.spawner.spawn(channel.then(move |result| match result {
                        Err(e) => {
                            warn!(logger, "A proxy channel aborted: {}", e);
                            Ok(())
                        }
                        Ok(()) => {
                            info!(logger, "A proxy channel terminated normally");
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
