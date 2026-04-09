//! Stdio transport — newline-delimited JSON over stdin/stdout.
//!
//! Used by IDE extensions (e.g., VS Code, JetBrains) that spawn the agent
//! as a child process and communicate over stdio.
//!
//! # Wire format
//!
//! One JSON object per line, terminated by `\n`:
//! ```text
//! {"jsonrpc":"2.0","id":1,"method":"agent.submit","params":{"text":"hello"}}\n
//! {"jsonrpc":"2.0","method":"agent.textDelta","params":{"text":"Hi"}}\n
//! ```

use async_trait::async_trait;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

use super::{Transport, TransportError};
use crate::protocol::RawMessage;

/// Stdio transport: reads from stdin, writes to stdout.
pub struct StdioTransport {
    reader: BufReader<tokio::io::Stdin>,
    writer: tokio::io::Stdout,
}

impl StdioTransport {
    pub fn new() -> Self {
        Self {
            reader: BufReader::new(tokio::io::stdin()),
            writer: tokio::io::stdout(),
        }
    }
}

impl Default for StdioTransport {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl Transport for StdioTransport {
    async fn read_message(&mut self) -> Result<Option<RawMessage>, TransportError> {
        loop {
            let mut line = String::new();
            let n = self.reader.read_line(&mut line).await?;
            if n == 0 {
                return Ok(None); // EOF
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
}

/// In-memory transport for testing — uses paired channels.
#[cfg(test)]
pub mod test_transport {
    use super::*;
    use tokio::sync::mpsc;

    pub struct ChannelTransport {
        rx: mpsc::Receiver<RawMessage>,
        tx: mpsc::Sender<RawMessage>,
    }

    impl ChannelTransport {
        /// Create a pair of connected transports.
        pub fn pair(capacity: usize) -> (Self, Self) {
            let (tx_a, rx_b) = mpsc::channel(capacity);
            let (tx_b, rx_a) = mpsc::channel(capacity);
            (
                Self { rx: rx_a, tx: tx_a },
                Self { rx: rx_b, tx: tx_b },
            )
        }
    }

    #[async_trait]
    impl Transport for ChannelTransport {
        async fn read_message(&mut self) -> Result<Option<RawMessage>, TransportError> {
            Ok(self.rx.recv().await)
        }

        async fn write_message(&mut self, msg: &RawMessage) -> Result<(), TransportError> {
            self.tx.send(msg.clone()).await
                .map_err(|_| TransportError::Closed)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::test_transport::ChannelTransport;
    use super::*;
    use crate::protocol::{Notification, Request};

    #[tokio::test]
    async fn channel_transport_roundtrip() {
        let (mut client, mut server) = ChannelTransport::pair(16);

        let req = Request::new(1, "agent.submit", Some(serde_json::json!({"text": "hi"})));
        let raw = RawMessage::from(req);

        client.write_message(&raw).await.unwrap();
        let received = server.read_message().await.unwrap().unwrap();

        assert_eq!(received.method.as_deref(), Some("agent.submit"));
        assert!(received.id.is_some());
    }

    #[tokio::test]
    async fn channel_transport_notification() {
        let (mut server, mut client) = ChannelTransport::pair(16);

        let notif = Notification::new("agent.textDelta", Some(serde_json::json!({"text": "hello"})));
        let raw = RawMessage::from(notif);

        server.write_message(&raw).await.unwrap();
        let received = client.read_message().await.unwrap().unwrap();

        assert_eq!(received.method.as_deref(), Some("agent.textDelta"));
        assert!(received.id.is_none());
    }

    #[tokio::test]
    async fn channel_transport_closed() {
        let (mut client, server) = ChannelTransport::pair(16);
        drop(server);

        let result = client.read_message().await.unwrap();
        assert!(result.is_none());
    }

    #[tokio::test]
    async fn channel_transport_write_to_closed() {
        let (client, mut server) = ChannelTransport::pair(16);
        drop(client);

        let raw = RawMessage::from(Notification::new("test", None));
        let result = server.write_message(&raw).await;
        assert!(result.is_err());
    }
}
