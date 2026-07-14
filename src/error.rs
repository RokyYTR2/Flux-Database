use crate::types::DataType;
use thiserror::Error;

#[derive(Debug, Error)]
pub enum FluxError {
    #[error("parse error: {0}")]
    Parse(String),
    #[error("table '{0}' already exists")]
    TableExists(String),
    #[error("table '{0}' not found")]
    TableNotFound(String),
    #[error("column '{column}' not found in table '{table}'")]
    ColumnNotFound { table: String, column: String },
    #[error("expected {expected} values, got {actual}")]
    ValueCountMismatch { expected: usize, actual: usize },
    #[error("type mismatch for column '{column}': expected {expected}, got {found:?}")]
    TypeMismatch {
        column: String,
        expected: DataType,
        found: Option<DataType>,
    },
    #[error("invalid schema: {0}")]
    InvalidSchema(String),
    #[error("constraint violation: {0}")]
    ConstraintViolation(String),
    #[error("transaction error: {0}")]
    Transaction(String),
    #[error("user '{0}' already exists")]
    UserExists(String),
    #[error("authentication failed")]
    AuthenticationFailed,
    #[error("authorization denied for user '{user}' to perform '{action}'")]
    AuthorizationDenied { user: String, action: String },
    #[error("configuration error: {0}")]
    Configuration(String),
    #[error("cryptography error: {0}")]
    Crypto(String),
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
    #[error("serialization error: {0}")]
    Serde(#[from] serde_json::Error),
    #[error("utf8 conversion error: {0}")]
    Utf8(#[from] std::string::FromUtf8Error),
    #[error("base64 decode error: {0}")]
    Base64(#[from] base64::DecodeError),
}

pub type Result<T> = std::result::Result<T, FluxError>;
