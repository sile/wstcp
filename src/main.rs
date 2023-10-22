extern crate clap;
#[macro_use]
extern crate trackable;

use async_std::net::TcpListener;
use clap::{Parser, ValueEnum};
use std::net::SocketAddr;
use wstcp::{Error, ProxyServer};

#[derive(Parser)]
struct Args {
    /// The TCP address of the real server.
    real_server_addr: SocketAddr,

    /// TCP address to which the WebSocket proxy bind.
    #[clap(long, default_value = "0.0.0.0:13892")]
    bind_addr: SocketAddr,
}

#[derive(Clone, Copy, PartialEq, Eq, ValueEnum)]
enum LogLevelArg {
    Debug,
    Info,
    Warning,
    Error,
}

fn main() -> trackable::result::TopLevelResult {
    env_logger::init();

    let args = Args::parse();
    let bind_addr = args.bind_addr;
    let tcp_server_addr = args.real_server_addr;

    async_std::task::block_on(async {
        let listener = track!(TcpListener::bind(bind_addr).await.map_err(Error::from))
            .expect("failed to start listening on the given proxy address");

        let proxy = ProxyServer::new(listener.incoming(), tcp_server_addr)
            .await
            .unwrap_or_else(|e| panic!("{}", e));
        proxy.await.unwrap_or_else(|e| panic!("{}", e));
    });
    Ok(())
}
