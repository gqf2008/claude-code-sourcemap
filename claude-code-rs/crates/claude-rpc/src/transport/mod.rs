//! Transport trait — async message read/write abstraction.
//!
//! Each transport (stdio, TCP, WebSocket) implements this trait to
//! provide the JSON-RPC server with a uniform message interface.

use async_trait::async_trait;

use crate::protocol::RawMessage;

/// Errors from transport read/write operations.
#[derive(Debug, thiserror::Error)]
pub enum TransportError {
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),
    #[error("JSON error: {0}")]
    Json(#[from] serde_json::Error),
    #[error("Connection closed")]
    Closed,
    #[error("Transport error: {0}")]
    Other(String),
}

/// Async transport for JSON-RPC messages.
///
/// Implementations handle framing (newline-delimited JSON, length-prefix, etc.)
/// and provide typed `RawMessage` objects.
#[async_trait]
pub trait Transport: Send + 'static {
    /// Read the next message from the transport.
    ///
    /// Returns `None` when the connection is cleanly closed.
    /// Returns `Err` on protocol/IO errors.
    async fn read_message(&mut self) -> Result<Option<RawMessage>, TransportError>;

    /// Write a message to the transport.
    async fn write_message(&mut self, msg: &RawMessage) -> Result<(), TransportError>;

    /// Close the transport gracefully.
    async fn close(&mut self) -> Result<(), TransportError> {
        Ok(())
    }
}

pub mod stdio;
pub mod tcp;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn transport_error_display() {
        let err = TransportError::Closed;
        assert_eq!(err.to_string(), "Connection closed");

        let err = TransportError::Other("test".into());
        assert_eq!(err.to_string(), "Transport error: test");
    }

    #[test]
    fn transport_error_from_io() {
        let io_err = std::io::Error::new(std::io::ErrorKind::BrokenPipe, "pipe broken");
        let err = TransportError::from(io_err);
        assert!(matches!(err, TransportError::Io(_)));
    }
}
