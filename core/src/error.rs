#[derive(Debug, thiserror::Error, uniffi::Error)]
pub enum CoreError {
    #[error("{message}")]
    Io { message: String },
    #[error("This file is not a readable book: {message}")]
    Invalid { message: String },
    #[error("Not found: {message}")]
    NotFound { message: String },
    #[error("This book is protected by DRM and cannot be opened.")]
    Protected,
}

impl CoreError {
    pub(crate) fn invalid(message: impl Into<String>) -> Self {
        CoreError::Invalid { message: message.into() }
    }
    pub(crate) fn not_found(message: impl Into<String>) -> Self {
        CoreError::NotFound { message: message.into() }
    }
}

impl From<std::io::Error> for CoreError {
    fn from(e: std::io::Error) -> Self {
        CoreError::Io { message: e.to_string() }
    }
}

impl From<zip::result::ZipError> for CoreError {
    fn from(e: zip::result::ZipError) -> Self {
        match e {
            zip::result::ZipError::FileNotFound => CoreError::not_found("archive entry"),
            zip::result::ZipError::Io(e) => e.into(),
            other => CoreError::invalid(other.to_string()),
        }
    }
}

impl From<roxmltree::Error> for CoreError {
    fn from(e: roxmltree::Error) -> Self {
        CoreError::invalid(format!("malformed XML ({e})"))
    }
}
