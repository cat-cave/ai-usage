//! Library error type. Domain errors are `thiserror`; glue IO uses `anyhow`.

use thiserror::Error;

#[derive(Debug, Error)]
pub enum Error {
    #[error("config error: {0}")]
    Config(String),
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
    #[error("http error: {0}")]
    Http(String),
    #[error("parse error: {0}")]
    Parse(String),
}

pub type Result<T> = std::result::Result<T, Error>;

/// Map any error to a redacted, safe-to-print reason string. Never includes the
/// URL (which may carry query secrets), headers, or response bodies that may
/// contain tokens.
pub fn safe_reason(e: &Error) -> String {
    match e {
        Error::Config(m) => format!("config: {}", redact(m)),
        Error::Io(e) => format!("io: {}", redact(&e.to_string())),
        Error::Http(m) => format!("http: {}", redact(m)),
        Error::Parse(m) => format!("parse: {}", redact(m)),
    }
}

/// Defer to the redactor for any message that might have touched a credential.
fn redact(s: &str) -> String {
    crate::redact::Redactor::redact_str(s)
}
