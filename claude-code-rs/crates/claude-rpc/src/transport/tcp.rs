//! TCP transport — newline-delimited JSON over a TCP socket.
//!
//! Used for daemon mode: the agent runs as a persistent process and
//! clients connect via TCP (typically on localhost).
//!
//! # Usage
//!
//! ```rust,ignore
//! let listener = TcpListener::bind("127.0.0.1:0").await?;
//! let port = listener.local_addr()?.port();
//!
//! // Accept connections and wrap them in TcpTransport
//! let (stream, _addr) = listener.accept().await?;
//! let transport = TcpTransport::new(stream);
//! ```

use async_trait::async_trait;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::TcpStream;

use super::{Transport, TransportError};
use crate::protocol::RawMessage;

/// TCP transport: wraps a `TcpStream` for newline-delimited JSON I/O.
pub struct TcpTransport {
    reader: BufReader<tokio::io::ReadHalf<TcpStream>>,
    writer: tokio::io::WriteHalf<TcpStream>,
}

impl TcpTransport {
    /// Wrap a connected TCP stream.
    pub fn new(stream: TcpStream) -> Self {
        let (read_half, write_half) = tokio::io::split(stream);
        Self {
            reader: BufReader::new(read_half),
            writer: write_half,
        }
    }
}

#[async_trait]
impl Transport for TcpTransport {
    async fn read_message(&mut self) -> Result<Option<RawMessage>, TransportError> {
        loop {
            let mut line = String::new();
            let n = self.reader.read_line(&mut line).await?;
            if n == 0 {
                return Ok(None); // Connection closed
            }
            let trimmed = line.trim();
            if trimmed.is_empty() {
                continue; // Skip empty lines without recursion
            }
            let msg: RawMessage = serde_json::from_str(trimmed)?;
            return Ok(Some(msg));
        }
    }

    async fn write_message(&mut self, msg: &RawMessage) -> Result<(), TransportError> {
        let json = serde_json::to_string(msg)?;
        self.writer.write_all(json.as_bytes()).await?;
        self.writer.write_all(b"\n").await?;
        self.writer.flush().await?;
        Ok(())
    }

    async fn close(&mut self) -> Result<(), TransportError> {
        self.writer.shutdown().await?;
        Ok(())
    }
}

/// Listener that accepts connections and produces `TcpTransport` instances.
pub struct TcpListener {
    inner: tokio::net::TcpListener,
}

impl TcpListener {
    /// Bind to an address and start listening.
    pub async fn bind(addr: &str) -> Result<Self, std::io::Error> {
        let inner = tokio::net::TcpListener::bind(addr).await?;
        Ok(Self { inner })
    }

    /// Get the local address this listener is bound to.
    pub fn local_addr(&self) -> Result<std::net::SocketAddr, std::io::Error> {
        self.inner.local_addr()
    }

    /// Accept the next connection and wrap it in a `TcpTransport`.
    pub async fn accept(&self) -> Result<(TcpTransport, std::net::SocketAddr), std::io::Error> {
        let (stream, addr) = self.inner.accept().await?;
        Ok((TcpTransport::new(stream), addr))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::{Notification, Request, RequestId, Response};

    #[tokio::test]
    async fn tcp_roundtrip() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();

        let server_task = tokio::spawn(async move {
            let (mut transport, _) = listener.accept().await.unwrap();
            let msg = transport.read_message().await.unwrap().unwrap();
            assert_eq!(msg.method.as_deref(), Some("agent.submit"));

            // Send response
            let resp = Response::success(msg.id.unwrap(), serde_json::json!({"ok": true}));
            let raw = RawMessage::from(resp);
            transport.write_message(&raw).await.unwrap();
        });

        let stream = TcpStream::connect(addr).await.unwrap();
        let mut client = TcpTransport::new(stream);

        let req = Request::new(1, "agent.submit", Some(serde_json::json!({"text": "hi"})));
        client.write_message(&RawMessage::from(req)).await.unwrap();

        let resp = client.read_message().await.unwrap().unwrap();
        assert!(resp.result.is_some());
        assert_eq!(resp.id, Some(RequestId::Number(1)));

        server_task.await.unwrap();
    }

    #[tokio::test]
    async fn tcp_notification_stream() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();

        let server_task = tokio::spawn(async move {
            let (mut transport, _) = listener.accept().await.unwrap();
            for i in 0..3 {
                let notif = Notification::new(
                    "agent.textDelta",
                    Some(serde_json::json!({"text": format!("chunk-{}", i)})),
                );
                transport.write_message(&RawMessage::from(notif)).await.unwrap();
            }
            transport.close().await.unwrap();
        });

        let stream = TcpStream::connect(addr).await.unwrap();
        let mut client = TcpTransport::new(stream);

        let mut count = 0;
        while let Ok(Some(msg)) = client.read_message().await {
            assert_eq!(msg.method.as_deref(), Some("agent.textDelta"));
            count += 1;
        }
        assert_eq!(count, 3);

        server_task.await.unwrap();
    }

    #[tokio::test]
    async fn tcp_connection_close_detection() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();

        let server_task = tokio::spawn(async move {
            let (transport, _) = listener.accept().await.unwrap();
            drop(transport); // Close immediately
        });

        let stream = TcpStream::connect(addr).await.unwrap();
        let mut client = TcpTransport::new(stream);

        // Give server time to close
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;

        let result = client.read_message().await.unwrap();
        assert!(result.is_none());

        server_task.await.unwrap();
    }
}
