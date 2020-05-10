use crate::channel::ProxyChannel;
use crate::{Error, Result};
use async_std::net::TcpListener;
use async_std::stream::Stream;
use slog::Logger;
use std::future::Future;
use std::net::SocketAddr;
use std::pin::Pin;
use std::task::Context;
use std::task::Poll;

/// WebSocket to TCP proxy server.
#[derive(Debug)]
pub struct ProxyServer {
    logger: Logger,
    proxy_addr: SocketAddr,
    real_server_addr: SocketAddr,
    listener: TcpListener,
}
impl ProxyServer {
    /// Makes a new `ProxyServer` instance.
    pub async fn new(
        logger: Logger,
        proxy_addr: SocketAddr,
        real_server_addr: SocketAddr,
    ) -> Result<Self> {
        let logger = logger.new(
            o!("proxy_addr" => proxy_addr.to_string(), "server_addr" => real_server_addr.to_string()),
        );
        info!(logger, "Starts a WebSocket proxy server");
        let listener = track!(TcpListener::bind(proxy_addr).await.map_err(Error::from))?;
        Ok(ProxyServer {
            logger,
            proxy_addr,
            real_server_addr,
            listener,
        })
    }
}
impl Future for ProxyServer {
    type Output = Result<()>;

    fn poll(self: Pin<&mut Self>, cx: &mut Context) -> Poll<Self::Output> {
        let this = self.get_mut();
        loop {
            match Pin::new(&mut this.listener.incoming()).poll_next(cx) {
                Poll::Pending => {
                    break;
                }
                Poll::Ready(None) => {
                    warn!(
                        this.logger,
                        "TCP socket for the WebSocket proxy server has been closed"
                    );
                    return Poll::Ready(Ok(()));
                }
                Poll::Ready(Some(Err(e))) => {
                    return Poll::Ready(Err(track!(Error::from(e))));
                }
                Poll::Ready(Some(Ok(stream))) => {
                    let addr = stream.peer_addr()?;
                    debug!(this.logger, "New client arrived: {}", addr);

                    let logger = this.logger.new(o!("client_addr" => addr.to_string()));
                    let channel = ProxyChannel::new(logger.clone(), stream, this.real_server_addr);
                    async_std::task::spawn(async move {
                        match channel.await {
                            Err(e) => {
                                warn!(logger, "A proxy channel aborted: {}", e);
                            }
                            Ok(()) => {
                                info!(logger, "A proxy channel terminated normally");
                            }
                        }
                    });
                }
            }
        }
        Poll::Pending
    }
}
