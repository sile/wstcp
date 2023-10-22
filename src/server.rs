use crate::channel::ProxyChannel;
use crate::{Error, Result};
use async_std::net::Incoming;
use async_std::stream::Stream;
use std::future::Future;
use std::net::SocketAddr;
use std::pin::Pin;
use std::task::Context;
use std::task::Poll;

/// WebSocket to TCP proxy server.
#[derive(Debug)]
pub struct ProxyServer<'a> {
    real_server_addr: SocketAddr,
    incoming: Incoming<'a>,
}
impl<'a> ProxyServer<'a> {
    /// Makes a new `ProxyServer` instance.
    pub async fn new(
        incoming: Incoming<'a>,
        real_server_addr: SocketAddr,
    ) -> Result<ProxyServer<'a>> {
        log::info!("Starts a WebSocket proxy server");
        Ok(ProxyServer {
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
                    log::warn!("TCP socket for the WebSocket proxy server has been closed");
                    return Poll::Ready(Ok(()));
                }
                Poll::Ready(Some(Err(e))) => {
                    return Poll::Ready(Err(track!(Error::from(e))));
                }
                Poll::Ready(Some(Ok(stream))) => {
                    let addr = stream.peer_addr()?;
                    log::debug!("New client arrived: {:?}", addr);

                    let channel = ProxyChannel::new(stream, this.real_server_addr);
                    async_std::task::spawn(async move {
                        match channel.await {
                            Err(e) => {
                                log::warn!("A proxy channel aborted: {}", e);
                            }
                            Ok(()) => {
                                log::info!("A proxy channel terminated normally");
                            }
                        }
                    });
                }
            }
        }
        Poll::Pending
    }
}
