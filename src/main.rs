extern crate clap;
#[macro_use]
extern crate trackable;

use async_std::net::TcpListener;
use clap::{Parser, ValueEnum};
use slog::o;
use sloggers::terminal::{Destination, TerminalLoggerBuilder};
use sloggers::types::SourceLocation;
use sloggers::Build;
use std::net::SocketAddr;
use wstcp::{Error, ProxyServer};

#[derive(Parser)]
struct Args {
    /// The TCP address of the real server.
    real_server_addr: SocketAddr,

    /// Log level.
    #[clap(long, default_value = "info")]
    log_level: LogLevelArg,

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
    let args = Args::parse();
    let bind_addr = args.bind_addr;
    let tcp_server_addr = args.real_server_addr;
    let log_level = match args.log_level {
        LogLevelArg::Debug => sloggers::types::Severity::Debug,
        LogLevelArg::Info => sloggers::types::Severity::Info,
        LogLevelArg::Warning => sloggers::types::Severity::Warning,
        LogLevelArg::Error => sloggers::types::Severity::Error,
    };
    let logger = track!(TerminalLoggerBuilder::new()
        .source_location(SourceLocation::None)
        .destination(Destination::Stderr)
        .level(log_level)
        .build())?;

    async_std::task::block_on(async {
        let logger = logger.new(
            o!("proxy_addr" => bind_addr.to_string(), "server_addr" => tcp_server_addr.to_string()),
        );
        let listener = track!(TcpListener::bind(bind_addr).await.map_err(Error::from))
            .expect("failed to start listening on the given proxy address");

        let proxy = ProxyServer::new(logger, listener.incoming(), tcp_server_addr)
            .await
            .unwrap_or_else(|e| panic!("{}", e));
        proxy.await.unwrap_or_else(|e| panic!("{}", e));
    });
    Ok(())
}
