use thiserror::Error;
use kobold_kernel::KernelError;
use kobold_types::BackendError;

#[derive(Debug, Error)]
pub enum RuntimeError {
    #[error("Kernel error: {0}")]
    Kernel(#[from] KernelError),

    #[error("Backend error: {0}")]
    Backend(#[from] BackendError),

    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),

    #[error("Configuration error: {0}")]
    Config(String),

    #[error("Transcript error: {0}")]
    Transcript(String),

    #[error("Serialization error: {0}")]
    Serialization(#[from] serde_json::Error),
}
