use crate::Error;
use crate::channel;
use std::net::SocketAddr;
use tokio::net::TcpListener;

/// WebSocket to TCP proxy server.
pub struct ProxyServer {
    listener: TcpListener,
    real_server_addr: SocketAddr,
}

impl ProxyServer {
    /// Binds the proxy listener and connects each incoming WebSocket client to
    /// the TCP address `real_server_addr`.
    pub async fn new(bind_addr: SocketAddr, real_server_addr: SocketAddr) -> Result<Self, Error> {
        let listener = TcpListener::bind(bind_addr).await?;
        log::info!(
            "Starts a WebSocket proxy server, bind_addr: {}, real_server_addr: {}",
            listener.local_addr()?,
            real_server_addr
        );
        Ok(Self {
            listener,
            real_server_addr,
        })
    }

    /// Returns the local address the listener is bound to.
    pub fn local_addr(&self) -> Result<SocketAddr, Error> {
        Ok(self.listener.local_addr()?)
    }

    /// Accepts incoming connections in a loop and serves each one in a
    /// dedicated task.
    pub async fn run(self) -> Result<(), Error> {
        loop {
            let (stream, addr) = self.listener.accept().await?;
            log::debug!("New client arrived: {addr}");

            let real_server_addr = self.real_server_addr;
            tokio::spawn(async move {
                match channel::run(stream, real_server_addr).await {
                    Err(e) => log::warn!("A proxy channel aborted: {e}"),
                    Ok(()) => log::info!("A proxy channel terminated normally"),
                }
            });
        }
    }
}
