wstcp
=====

[![wstcp](http://meritbadge.herokuapp.com/wstcp)](https://crates.io/crates/wstcp)
[![Documentation](https://docs.rs/wstcp/badge.svg)](https://docs.rs/wstcp)
[![Actions Status](https://github.com/sile/wstcp/workflows/CI/badge.svg)](https://github.com/sile/wstcp/actions)
[![Coverage Status](https://coveralls.io/repos/github/sile/wstcp/badge.svg?branch=main)](https://coveralls.io/github/sile/wstcp?branch=main)
[![License: MIT](https://img.shields.io/badge/license-MIT-blue.svg)](LICENSE)

WebSocket to TCP proxy written in Rust.

Install
--------

### Precompiled binaries

A precompiled binary for Linux environment is available in the [releases] page.

```console
$ curl -L https://github.com/sile/wstcp/releases/download/0.2.0/wstcp-0.2.0.linux -o wstcp
$ chmod +x wstcp
$ ./wstcp -h
wstcp 0.2.0
WebSocket to TCP proxy server

USAGE:
    wstcp [OPTIONS] <REAL_SERVER_ADDR>

FLAGS:
    -h, --help       Prints help information
    -V, --version    Prints version information

OPTIONS:
        --bind-addr <BIND_ADDR>    TCP address to which the WebSocket proxy bind [default: 0.0.0.0:13892]
        --log-level <LOG_LEVEL>     [default: info]  [possible values: debug, info, warning, error]

ARGS:
    <REAL_SERVER_ADDR>    The TCP address of the real server
```

### Using Cargo

If you have already installed [Cargo][cargo], you can install `wstcp` easily in the following command:

```console
$ cargo install wstcp
```

[cargo]: https://doc.rust-lang.org/cargo/
[releases]: https://github.com/sile/wstcp/releases

Examples
---------

Run `wstcp` in a terminal:

```console
$ wstcp 127.0.0.1:3000
Apr 22 23:21:20.717 INFO Starts a WebSocket proxy server, server_addr: 127.0.0.1:3000, proxy_addr: 0.0.0.0:13892
```

Run a TCP server (in this example `nc` is used) in another terminal:

```console
$ nc -l 127.0.0.1 -p 3000
```

Use [ws](https://github.com/hashrocket/ws) to launch a WebSocket client:

```console
$ ws ws://localhost:13892/
> foo # Enter "foo" and press the Enter key
```

After this, the "foo" string is displayed on the terminal running `nc`.

References
----------

- [RFC 6455] The WebSocket Protocol

[RFC 6455]: https://tools.ietf.org/html/rfc6455
