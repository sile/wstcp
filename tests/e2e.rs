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

#[tokio::test]
async fn test_text_message() {
    // Exercises the TextMessage branch in channel.rs: the proxy must forward
    // the UTF-8 bytes of a text frame to the TCP backend.
    let echo_addr = start_echo_server().await;
    let proxy_addr = start_proxy(echo_addr).await;

    let (mut conn, mut stream) = ws_connect(proxy_addr).await;

    conn.send_text("hello, text!").unwrap();
    flush_outputs(&mut conn, &mut stream).await;

    // The TCP echo server returns the raw bytes, so the proxy re-frames them
    // as binary on the way back.
    let event = feed_until_event(&mut conn, &mut stream, |e| {
        matches!(e, ConnectionEvent::BinaryMessage(_))
    })
    .await;

    match event {
        Some(ConnectionEvent::BinaryMessage(data)) => {
            assert_eq!(data, b"hello, text!");
        }
        other => panic!("expected BinaryMessage echo, got {:?}", other),
    }
}

#[tokio::test]
async fn test_large_payload() {
    // 64KB payload — well above the 4KB internal read buffer, so the relay
    // arrives as multiple BinaryMessage events that must reassemble exactly.
    let echo_addr = start_echo_server().await;
    let proxy_addr = start_proxy(echo_addr).await;

    let (mut conn, mut stream) = ws_connect(proxy_addr).await;

    let payload: Vec<u8> = (0..65536).map(|i| (i % 256) as u8).collect();
    conn.send_binary(&payload).unwrap();
    flush_outputs(&mut conn, &mut stream).await;

    let mut received: Vec<u8> = Vec::new();
    let mut buf = vec![0u8; 4096];
    let result = timeout(Duration::from_secs(5), async {
        while received.len() < payload.len() {
            while let Some(event) = conn.poll_event() {
                if let ConnectionEvent::BinaryMessage(data) = event {
                    received.extend_from_slice(&data);
                }
            }
            if received.len() >= payload.len() {
                break;
            }
            let n = stream.read(&mut buf).await.unwrap();
            assert!(n > 0, "unexpected EOF from proxy");
            conn.feed_recv_buf(&buf[..n], now()).unwrap();
            flush_outputs(&mut conn, &mut stream).await;
        }
    })
    .await;

    assert!(result.is_ok(), "timed out waiting for echoed data");
    assert_eq!(received, payload);
}

async fn start_immediate_close_server() -> SocketAddr {
    // Accepts a connection then drops the stream immediately, so the proxy
    // observes an EOF on the real-side reader.
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        loop {
            let Ok((stream, _)) = listener.accept().await else {
                break;
            };
            drop(stream);
        }
    });
    addr
}

#[tokio::test]
async fn test_real_server_closes_first() {
    // The real TCP server closes its end. The proxy should send a Close frame
    // to the WebSocket client.
    let real_addr = start_immediate_close_server().await;
    let proxy_addr = start_proxy(real_addr).await;

    let (mut conn, mut stream) = ws_connect(proxy_addr).await;

    let event = feed_until_event(&mut conn, &mut stream, |e| {
        matches!(e, ConnectionEvent::Close { .. })
    })
    .await;

    match event {
        Some(ConnectionEvent::Close { code, .. }) => {
            assert_eq!(code, Some(CloseCode::NORMAL));
        }
        other => panic!("expected Close event, got {:?}", other),
    }
}

#[tokio::test]
async fn test_concurrent_connections() {
    // Several clients use the same proxy in parallel. Catches accidental
    // sharing of per-connection state across tasks.
    let echo_addr = start_echo_server().await;
    let proxy_addr = start_proxy(echo_addr).await;

    let mut handles = Vec::new();
    for i in 0..5u8 {
        let handle = tokio::spawn(async move {
            let (mut conn, mut stream) = ws_connect(proxy_addr).await;
            let payload = vec![i; 32];
            conn.send_binary(&payload).unwrap();
            flush_outputs(&mut conn, &mut stream).await;

            let event = feed_until_event(&mut conn, &mut stream, |e| {
                matches!(e, ConnectionEvent::BinaryMessage(_))
            })
            .await;

            match event {
                Some(ConnectionEvent::BinaryMessage(data)) => assert_eq!(data, payload),
                other => panic!("client {i}: expected BinaryMessage, got {:?}", other),
            }
        });
        handles.push(handle);
    }

    for h in handles {
        h.await.unwrap();
    }
}
