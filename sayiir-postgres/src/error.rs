//! Error mapping from sqlx to `BackendError`.

use sayiir_persistence::BackendError;

/// Map a [`sqlx::Error`] into a [`BackendError`].
impl From<sqlx::Error> for PgError {
    fn from(e: sqlx::Error) -> Self {
        Self(e)
    }
}

/// Newtype wrapper so we can implement `Into<BackendError>` without orphan rules.
pub(crate) struct PgError(pub sqlx::Error);

impl From<PgError> for BackendError {
    fn from(e: PgError) -> Self {
        match e.0 {
            sqlx::Error::RowNotFound => BackendError::NotFound(e.0.to_string()),
            other => BackendError::Backend(other.to_string()),
        }
    }
}
