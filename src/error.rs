use thiserror::Error;

#[derive(Error, Debug)]
pub enum EmailClError {
    #[error("Configuration error: {0}")]
    Config(String),

    #[error("Database error: {0}")]
    Database(#[from] rusqlite::Error),

    #[error("HTTP request error: {0}")]
    Http(#[from] reqwest::Error),

    #[error("Authentication error: {0}")]
    Auth(String),

    #[error("Token error: {0}")]
    Token(String),

    #[error("Graph API error: {0}")]
    GraphApi(String),

    #[error("Ollama error: {0}")]
    Ollama(String),

    #[error("Classification error: {0}")]
    Classification(String),

    #[error("JSON parse error: {0}")]
    Json(#[from] serde_json::Error),

    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),

    #[error("Notification error: {0}")]
    Notification(String),

    #[error("Keyring error: {0}")]
    Keyring(String),
}

pub type Result<T> = std::result::Result<T, EmailClError>;
