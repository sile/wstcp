use crate::channel::ProxyChannel;
use crate::{Error, Result};
use async_std::net::Incoming;
use async_std::stream::Stream;
use slog::Logger;
use std::future::Future;
use std::net::SocketAddr;
use std::pin::Pin;
use std::task::Context;
use std::task::Poll;

/// WebSocket to TCP proxy server.
#[derive(Debug)]
pub struct ProxyServer<'a> {
    logger: Logger,
    real_server_addr: SocketAddr,
    incoming: Incoming<'a>,
}
impl<'a> ProxyServer<'a> {
    /// Makes a new `ProxyServer` instance.
    pub async fn new(
        logger: Logger,
        incoming: Incoming<'a>,
        real_server_addr: SocketAddr,
    ) -> Result<ProxyServer<'a>> {
        info!(logger, "Starts a WebSocket proxy server");
        Ok(ProxyServer {
            logger,
            real_server_addr,
            incoming,
        })
    }
}
impl<'a> Future for ProxyServer<'a> {
    type Output = Result<()>;

    fn poll(self: Pin<&mut Self>, cx: &mut Context) -> Poll<Self::Output> {
        let this = self.get_mut();
        loop {
            match Pin::new(&mut this.incoming).poll_next(cx) {
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
                    debug!(this.logger, "New client arrived: {:?}", addr);

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
