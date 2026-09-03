//! Error handling module
//!
//! Defines project-specific error types and error handling logic

use thiserror::Error;

/// Project error type
#[derive(Error, Debug)]
pub enum TcpKaliError {
    /// IO error
    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),

    /// Network connection error
    #[error("Network connection error: {0}")]
    Network(String),

    /// WebSocket error
    #[error("WebSocket error: {0}")]
    WebSocket(String),

    /// Configuration error
    #[error("Configuration error: {0}")]
    Config(String),

    /// Statistics recording error
    #[error("Statistics recording error: {0}")]
    #[allow(dead_code)]
    Stats(String),

    /// Task execution error
    #[error("Task execution error: {0}")]
    Task(String),

    /// Timeout error
    #[error("Timeout error: {0}")]
    Timeout(String),

    /// Parse error
    #[error("Parse error: {0}")]
    Parse(String),
}

/// Project result type alias
#[allow(dead_code)]
pub type Result<T> = std::result::Result<T, TcpKaliError>;

/// Conversion from tungstenite error
impl From<tungstenite::Error> for TcpKaliError {
    fn from(err: tungstenite::Error) -> Self {
        TcpKaliError::WebSocket(err.to_string())
    }
}

/// Conversion from address parse error
impl From<std::net::AddrParseError> for TcpKaliError {
    fn from(err: std::net::AddrParseError) -> Self {
        TcpKaliError::Parse(err.to_string())
    }
}

/// Convenient error creation functions
#[allow(dead_code)]
impl TcpKaliError {
    /// Create network error
    pub fn network(msg: impl Into<String>) -> Self {
        TcpKaliError::Network(msg.into())
    }

    /// Create configuration error
    pub fn config(msg: impl Into<String>) -> Self {
        TcpKaliError::Config(msg.into())
    }

    /// Create task error
    pub fn task(msg: impl Into<String>) -> Self {
        TcpKaliError::Task(msg.into())
    }

    /// Create timeout error
    pub fn timeout(msg: impl Into<String>) -> Self {
        TcpKaliError::Timeout(msg.into())
    }
}
