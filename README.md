wstcp
=====

[![wstcp](https://img.shields.io/crates/v/wstcp.svg)](https://crates.io/crates/wstcp)
[![Documentation](https://docs.rs/wstcp/badge.svg)](https://docs.rs/wstcp)
[![Actions Status](https://github.com/sile/wstcp/workflows/CI/badge.svg)](https://github.com/sile/wstcp/actions)
[![License: MIT](https://img.shields.io/badge/license-MIT-blue.svg)](LICENSE)

WebSocket to TCP proxy written in Rust.

Install
--------

### Precompiled binaries

A precompiled binary for Linux environment is available in the [releases] page.

```console
$ curl -L https://github.com/sile/wstcp/releases/download/0.2.0/wstcp-0.2.0.linux -o wstcp
$ chmod +x wstcp
$ ./wstcp --help
WebSocket to TCP proxy server

Usage: wstcp [OPTIONS] <REAL_SERVER_ADDR>

Example:
  $ wstcp 127.0.0.1:3000

Arguments:
  <REAL_SERVER_ADDR>
    The TCP address of the real server

Options:
  --version
    Print version

  --help, -h
    Print help ('--help' for full help, '-h' for summary)

  --bind-addr <ADDR>
    TCP address to which the WebSocket proxy binds
    [default: 0.0.0.0:13892]
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

Run `wstcp` in a terminal (set `RUST_LOG` to see info-level logs):

```console
$ RUST_LOG=info wstcp 127.0.0.1:3000
[2026-05-22T12:00:00Z INFO  wstcp::server] Starts a WebSocket proxy server, bind_addr: 0.0.0.0:13892, real_server_addr: 127.0.0.1:3000
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
