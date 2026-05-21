use crate::Error;
use shiguredo_websocket::{
    CloseCode, ConnectionEvent, ConnectionOutput, ServerConnectionOptions, TimerId,
    WebSocketServerConnection,
};
use std::net::SocketAddr;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;
use tokio::net::tcp::{OwnedReadHalf, OwnedWriteHalf};
use tokio::time::{Duration, Instant};

const BUF_SIZE: usize = 4096;
const REAL_SERVER_CONNECT_TIMEOUT: Duration = Duration::from_secs(5);

pub async fn run(ws_stream: TcpStream, real_server_addr: SocketAddr) -> Result<(), Error> {
    ws_stream.set_nodelay(true)?;
    let (mut ws_reader, mut ws_writer) = ws_stream.into_split();

    let mut ws_conn = WebSocketServerConnection::new(ServerConnectionOptions::new());
    let mut real_reader: Option<OwnedReadHalf> = None;
    let mut real_writer: Option<OwnedWriteHalf> = None;
    let mut timers = TimerManager::new();
    let mut ws_buf = vec![0u8; BUF_SIZE];
    let mut real_buf = vec![0u8; BUF_SIZE];

    log::info!("New proxy channel is created");

    loop {
        // 1. Drain pending outputs from the WebSocket state machine.
        while let Some(output) = ws_conn.poll_output() {
            match output {
                ConnectionOutput::SendData(data) => {
                    ws_writer.write_all(&data).await?;
                }
                ConnectionOutput::SetTimer {
                    id,
                    duration_millis,
                } => {
                    timers.set(id, duration_millis);
                }
                ConnectionOutput::ClearTimer { id } => {
                    timers.clear(id);
                }
                ConnectionOutput::CloseConnection => {
                    log::info!("WebSocket channel has been closed");
                    return Ok(());
                }
            }
        }

        // 2. Process pending events.
        while let Some(event) = ws_conn.poll_event() {
            match event {
                ConnectionEvent::Connected {
                    protocol,
                    extensions,
                } => {
                    log::info!(
                        "WebSocket handshake succeeded, protocol: {protocol:?}, extensions: {extensions:?}"
                    );
                }
                ConnectionEvent::BinaryMessage(data) => {
                    if let Some(w) = real_writer.as_mut()
                        && let Err(e) = w.write_all(&data).await
                    {
                        log::warn!("Real server write error: {e}");
                        real_reader = None;
                        real_writer = None;
                        ws_conn.close(CloseCode::GOING_AWAY, "")?;
                    }
                }
                ConnectionEvent::TextMessage(text) => {
                    if let Some(w) = real_writer.as_mut()
                        && let Err(e) = w.write_all(text.as_bytes()).await
                    {
                        log::warn!("Real server write error: {e}");
                        real_reader = None;
                        real_writer = None;
                        ws_conn.close(CloseCode::GOING_AWAY, "")?;
                    }
                }
                ConnectionEvent::Close { code, reason } => {
                    log::info!("Received Close frame: code={code:?}, reason={reason:?}");
                    real_reader = None;
                    real_writer = None;
                }
                ConnectionEvent::Ping(data) => {
                    log::debug!("Received Ping frame: {data:?}");
                }
                ConnectionEvent::Pong(data) => {
                    log::debug!("Received Pong frame: {data:?}");
                }
                ConnectionEvent::StateChanged(state) => {
                    log::debug!("Connection state changed: {state:?}");
                }
                ConnectionEvent::Error(msg) => {
                    log::warn!("WebSocket error event: {msg}");
                }
            }
        }

        // 3. If the handshake request has arrived but not yet been accepted,
        //    connect to the real server and accept or reject.
        //
        // Note: accept_handshake_auto() does not validate Origin/Path. wstcp
        // is a transparent TCP proxy by design, so this is intentional, but
        // it means CSWSH is possible in browser+cookie contexts.
        if real_writer.is_none() && ws_conn.handshake_request().is_some() {
            let connect = tokio::time::timeout(
                REAL_SERVER_CONNECT_TIMEOUT,
                TcpStream::connect(real_server_addr),
            )
            .await;
            match connect {
                Ok(Ok(real)) => {
                    log::debug!("Connected to the real server");
                    real.set_nodelay(true)?;
                    let (r, w) = real.into_split();
                    real_reader = Some(r);
                    real_writer = Some(w);
                    ws_conn.accept_handshake_auto()?;
                }
                Ok(Err(e)) => {
                    log::warn!("Cannot connect to the real server: {e}");
                    ws_conn.reject_handshake(503, "Service Unavailable", &[])?;
                }
                Err(_) => {
                    log::warn!(
                        "Timed out connecting to the real server after {:?}",
                        REAL_SERVER_CONNECT_TIMEOUT
                    );
                    ws_conn.reject_handshake(504, "Gateway Timeout", &[])?;
                }
            }
            continue;
        }

        // 4. Wait for the next event (WebSocket bytes, real-server bytes, or timer).
        tokio::select! {
            result = ws_reader.read(&mut ws_buf) => {
                let n = result?;
                if n == 0 {
                    log::info!("TCP stream for WebSocket has been closed");
                    return Ok(());
                }
                ws_conn.feed_recv_buf(&ws_buf[..n])?;
            }
            result = read_optional(real_reader.as_mut(), &mut real_buf) => {
                match result {
                    Ok(0) => {
                        log::info!("TCP stream for a real server has been closed");
                        real_reader = None;
                        real_writer = None;
                        ws_conn.close(CloseCode::NORMAL, "")?;
                    }
                    Ok(n) => {
                        ws_conn.send_binary(&real_buf[..n])?;
                    }
                    Err(e) => {
                        log::warn!("Real server read error: {e}");
                        real_reader = None;
                        real_writer = None;
                        ws_conn.close(CloseCode::GOING_AWAY, "")?;
                    }
                }
            }
            _ = timers.wait_next() => {
                for id in timers.expired() {
                    ws_conn.handle_timer(id)?;
                }
            }
        }
    }
}

async fn read_optional(
    reader: Option<&mut OwnedReadHalf>,
    buf: &mut [u8],
) -> std::io::Result<usize> {
    match reader {
        Some(r) => r.read(buf).await,
        None => std::future::pending().await,
    }
}

struct TimerManager {
    timers: Vec<(TimerId, Instant)>,
}

impl TimerManager {
    fn new() -> Self {
        Self { timers: Vec::new() }
    }

    fn set(&mut self, id: TimerId, duration_millis: u64) {
        let deadline = Instant::now() + Duration::from_millis(duration_millis);
        self.timers.retain(|(tid, _)| *tid != id);
        self.timers.push((id, deadline));
    }

    fn clear(&mut self, id: TimerId) {
        self.timers.retain(|(tid, _)| *tid != id);
    }

    fn next_deadline(&self) -> Option<Instant> {
        self.timers.iter().map(|(_, d)| *d).min()
    }

    async fn wait_next(&self) {
        match self.next_deadline() {
            Some(deadline) => tokio::time::sleep_until(deadline).await,
            None => std::future::pending().await,
        }
    }

    fn expired(&mut self) -> Vec<TimerId> {
        let now = Instant::now();
        let mut out = Vec::new();
        self.timers.retain(|(id, deadline)| {
            if *deadline <= now {
                out.push(*id);
                false
            } else {
                true
            }
        });
        out
    }
}
