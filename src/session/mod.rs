pub mod environment;
pub mod farm;
pub mod health;
pub mod process;
pub mod seat;

fn io(error: std::io::Error) -> crate::error::BackendError {
    crate::error::BackendError::Failed(error.to_string())
}
