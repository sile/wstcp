#[macro_use]
extern crate clap;
extern crate fibers;
extern crate futures;
extern crate sloggers;
#[macro_use]
extern crate trackable;
extern crate wstcp;

use clap::Arg;
use fibers::{Executor, Spawn, ThreadPoolExecutor};
use futures::Future;
use sloggers::terminal::{Destination, TerminalLoggerBuilder};
use sloggers::types::SourceLocation;
use sloggers::Build;
use std::net::{SocketAddr, ToSocketAddrs};
use wstcp::ProxyServer;

macro_rules! try_parse {
    ($expr:expr) => {
        track_try_unwrap!(track_any_err!($expr.parse()))
    };
}

fn main() {
    let matches = app_from_crate!()
        .arg(
            Arg::with_name("REAL_SERVER_ADDR")
                .help("The TCP address of the real server")
                .index(1)
                .takes_value(true)
                .required(true),
        )
        .arg(
            Arg::with_name("LOG_LEVEL")
                .long("log-level")
                .takes_value(true)
                .default_value("info")
                .possible_values(&["debug", "info", "warning", "error"]),
        )
        .arg(
            Arg::with_name("BIND_ADDR")
                .help("TCP address to which the WebSocket proxy bind")
                .long("bind-addr")
                .takes_value(true)
                .default_value("0.0.0.0:13892"),
        )
        .get_matches();

    let bind_addr: SocketAddr = try_parse!(matches.value_of("BIND_ADDR").unwrap());
    let tcp_server_addr: SocketAddr = track_try_unwrap!(track_any_err!(matches
        .value_of("REAL_SERVER_ADDR")
        .unwrap()
        .to_socket_addrs()))
    .next()
    .unwrap();
    let log_level = try_parse!(matches.value_of("LOG_LEVEL").unwrap());
    let logger = track_try_unwrap!(TerminalLoggerBuilder::new()
        .source_location(SourceLocation::None)
        .destination(Destination::Stderr)
        .level(log_level)
        .build());

    let executor = track_try_unwrap!(track_any_err!(ThreadPoolExecutor::new()));
    let proxy = ProxyServer::new(logger, executor.handle(), bind_addr, tcp_server_addr);
    executor.spawn(proxy.map_err(|e| panic!("{}", e)));
    track_try_unwrap!(track_any_err!(executor.run()))
}
