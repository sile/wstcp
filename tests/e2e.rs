use shiguredo_websocket::{
    ClientConnectionOptions, CloseCode, ConnectionEvent, ConnectionOutput, RandomSource, Timestamp,
    WebSocketClientConnection,
};
use std::net::SocketAddr;
use std::time::SystemTime;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::time::{Duration, timeout};

struct TestRandom;

impl RandomSource for TestRandom {
    fn masking_key(&mut self) -> [u8; 4] {
        [1, 2, 3, 4]
    }

    fn nonce(&mut self) -> [u8; 16] {
        *b"test-nonce-abcde"
    }
}

fn now() -> Timestamp {
    let millis = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .unwrap()
        .as_millis() as u64;
    Timestamp::from_millis(millis)
}

async fn start_echo_server() -> SocketAddr {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        loop {
            let Ok((mut stream, _)) = listener.accept().await else {
                break;
            };
            tokio::spawn(async move {
                let mut buf = [0u8; 4096];
                loop {
                    let n = match stream.read(&mut buf).await {
                        Ok(0) | Err(_) => break,
                        Ok(n) => n,
                    };
                    if stream.write_all(&buf[..n]).await.is_err() {
                        break;
                    }
                }
            });
        }
    });
    addr
}

async fn start_proxy(real_server_addr: SocketAddr) -> SocketAddr {
    let proxy = wstcp::ProxyServer::new("127.0.0.1:0".parse().unwrap(), real_server_addr)
        .await
        .unwrap();
    let addr = proxy.local_addr().unwrap();
    tokio::spawn(async move {
        let _ = proxy.run().await;
    });
    addr
}

async fn flush_outputs(conn: &mut WebSocketClientConnection<TestRandom>, stream: &mut TcpStream) {
    while let Some(output) = conn.poll_output() {
        if let ConnectionOutput::SendData(data) = output {
            stream.write_all(&data).await.unwrap();
        }
    }
}

async fn feed_until_event(
    conn: &mut WebSocketClientConnection<TestRandom>,
    stream: &mut TcpStream,
    mut predicate: impl FnMut(&ConnectionEvent) -> bool,
) -> Option<ConnectionEvent> {
    let mut buf = vec![0u8; 4096];
    timeout(Duration::from_secs(5), async {
        loop {
            while let Some(event) = conn.poll_event() {
                if predicate(&event) {
                    return event;
                }
            }
            let n = stream.read(&mut buf).await.unwrap();
            assert!(n > 0, "unexpected EOF from proxy");
            conn.feed_recv_buf(&buf[..n], now()).unwrap();
            flush_outputs(conn, stream).await;
        }
    })
    .await
    .ok()
}

async fn ws_connect(proxy_addr: SocketAddr) -> (WebSocketClientConnection<TestRandom>, TcpStream) {
    let mut stream = TcpStream::connect(proxy_addr).await.unwrap();
    let options = ClientConnectionOptions::new("127.0.0.1", "/");
    let mut conn = WebSocketClientConnection::new(options, TestRandom);

    conn.connect().unwrap();
    flush_outputs(&mut conn, &mut stream).await;

    let event = feed_until_event(&mut conn, &mut stream, |e| {
        matches!(e, ConnectionEvent::Connected { .. })
    })
    .await;
    assert!(
        event.is_some(),
        "WebSocket handshake did not complete within timeout"
    );

    (conn, stream)
}

#[tokio::test]
async fn test_binary_echo() {
    let echo_addr = start_echo_server().await;
    let proxy_addr = start_proxy(echo_addr).await;

    let (mut conn, mut stream) = ws_connect(proxy_addr).await;

    let payload = b"hello, wstcp!";
    conn.send_binary(payload).unwrap();
    flush_outputs(&mut conn, &mut stream).await;

    let event = feed_until_event(&mut conn, &mut stream, |e| {
        matches!(e, ConnectionEvent::BinaryMessage(_))
    })
    .await;

    match event {
        Some(ConnectionEvent::BinaryMessage(data)) => assert_eq!(data, payload),
        other => panic!("expected BinaryMessage, got {:?}", other),
    }
}

#[tokio::test]
async fn test_multiple_messages() {
    let echo_addr = start_echo_server().await;
    let proxy_addr = start_proxy(echo_addr).await;

    let (mut conn, mut stream) = ws_connect(proxy_addr).await;

    for msg in [
        b"first".as_slice(),
        b"second".as_slice(),
        b"third".as_slice(),
    ] {
        conn.send_binary(msg).unwrap();
        flush_outputs(&mut conn, &mut stream).await;

        let event = feed_until_event(&mut conn, &mut stream, |e| {
            matches!(e, ConnectionEvent::BinaryMessage(_))
        })
        .await;

        match event {
            Some(ConnectionEvent::BinaryMessage(data)) => assert_eq!(data, msg),
            other => panic!("expected BinaryMessage, got {:?}", other),
        }
    }
}

#[tokio::test]
async fn test_close_handshake() {
    let echo_addr = start_echo_server().await;
    let proxy_addr = start_proxy(echo_addr).await;

    let (mut conn, mut stream) = ws_connect(proxy_addr).await;

    conn.close(CloseCode::NORMAL, "bye").unwrap();
    flush_outputs(&mut conn, &mut stream).await;

    let event = feed_until_event(&mut conn, &mut stream, |e| {
        matches!(e, ConnectionEvent::Close { .. })
    })
    .await;

    assert!(
        event.is_some(),
        "did not receive Close event within timeout"
    );
}
