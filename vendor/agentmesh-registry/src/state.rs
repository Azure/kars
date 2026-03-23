use sqlx::PgPool;
use std::sync::atomic::{AtomicBool, Ordering};

/// Shared application state for graceful startup
///
/// This enables the HTTP server to start immediately while database
/// connection and migrations happen in the background.
pub struct AppState {
    /// Database connection pool (lazy - connections made on demand)
    pub pool: PgPool,
    /// True once database is connected and migrations are complete
    ready: AtomicBool,
}

impl AppState {
    /// Create new AppState with the given pool, initially not ready
    pub fn new(pool: PgPool) -> Self {
        Self {
            pool,
            ready: AtomicBool::new(false),
        }
    }

    /// Check if the application is ready to serve requests
    pub fn is_ready(&self) -> bool {
        self.ready.load(Ordering::SeqCst)
    }

    /// Mark the application as ready (called after successful initialization)
    pub fn set_ready(&self) {
        self.ready.store(true, Ordering::SeqCst);
    }

    /// Get pool reference if ready, otherwise return a "not ready" error response
    /// Use this in handlers that require database access
    pub fn require_ready(&self) -> Result<&PgPool, NotReadyError> {
        if self.is_ready() {
            Ok(&self.pool)
        } else {
            Err(NotReadyError)
        }
    }
}

/// Error returned when application is not yet ready
#[derive(Debug)]
pub struct NotReadyError;

impl std::fmt::Display for NotReadyError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Service is starting up")
    }
}

impl std::error::Error for NotReadyError {}
