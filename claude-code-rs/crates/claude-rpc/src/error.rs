//! Error types for the RPC layer.

use crate::protocol::RpcError;

/// Convenience result type for RPC operations.
pub type RpcResult<T> = Result<T, RpcError>;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::error_codes;

    #[test]
    fn rpc_result_ok() {
        let result: RpcResult<i32> = Ok(42);
        assert_eq!(result.unwrap(), 42);
    }

    #[test]
    fn rpc_result_err() {
        let result: RpcResult<i32> = Err(RpcError::new(
            error_codes::INTERNAL_ERROR,
            "test error",
        ));
        assert!(result.is_err());
    }
}
