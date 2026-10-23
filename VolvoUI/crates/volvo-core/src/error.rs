use std::io;

#[derive(Debug, thiserror::Error)]
pub enum CoreError {
    /// A safety or precondition check refused the action. The text is shown to the user as is.
    #[error("{0}")]
    Refused(String),
    #[error("{0}")]
    Invalid(String),
    #[error("Another job is already running")]
    Busy,
    #[error("No job is running")]
    NotRunning,
    #[error("Could not start {path}: {source}")]
    Spawn { path: String, source: io::Error },
    #[error("{context}: {source}")]
    Io { context: String, source: io::Error },
}

pub type Result<T> = std::result::Result<T, CoreError>;

pub(crate) fn io_err(context: impl Into<String>) -> impl FnOnce(io::Error) -> CoreError {
    let context = context.into();
    move |source| CoreError::Io { context, source }
}
