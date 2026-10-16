use serde::Serialize;
use serde_json::{Value, json};

pub type Result<T> = std::result::Result<T, Error>;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("{message}")]
    Rule {
        code: &'static str,
        message: String,
        entities: Value,
    },
    #[error("{0}")]
    Invalid(String),
    #[error("{0}")]
    Conflict(String),
    #[error("Expected version {expected}; current version is {actual}")]
    Version { expected: i64, actual: i64 },
    #[error("Entity not found in the authorized ledger")]
    NotFound,
    #[error("Required OAuth scope is missing")]
    Forbidden,
    #[error("The idempotency key was already used for a different request")]
    Idempotency,
    #[error("The ledger changed; restart pagination")]
    CursorStale,
    #[error("{0}")]
    Limit(String),
    #[error("Amount is outside the supported integer range")]
    Overflow,
    #[error(transparent)]
    Database(#[from] sqlx::Error),
    #[error(transparent)]
    Json(#[from] serde_json::Error),
}

#[derive(Serialize)]
pub struct ErrorBody {
    pub code: &'static str,
    pub message: String,
    pub next_actions: Vec<&'static str>,
}

impl Error {
    pub fn body(&self) -> Value {
        if let Self::Rule {
            code,
            message,
            entities,
        } = self
        {
            return json!({"code":code,"message":message,"entities":entities,"next_actions":["Read the affected records, resolve the conflict and retry atomically"]});
        }
        if let Self::Database(sqlx::Error::Database(error)) = self {
            match error.code().as_deref() {
                Some("23503" | "23505" | "23514") => {
                    return json!(ErrorBody {
                        code: "CONSTRAINT_CONFLICT",
                        message: "A database ledger constraint rejected the batch".into(),
                        next_actions: vec![
                            "Read the affected entities and correct the whole batch"
                        ],
                    });
                }
                Some("40001" | "40P01" | "55P03" | "57014") => {
                    return json!(ErrorBody {
                        code: "DATABASE_BUSY",
                        message: "The database is temporarily busy".into(),
                        next_actions: vec!["Retry with the original idempotency key"],
                    });
                }
                _ => {}
            }
        }
        let (code, action) = match self {
            Self::Rule { .. } => unreachable!(),
            Self::Invalid(_) | Self::Json(_) => {
                ("INVALID_FIELD", "Correct the request using the tool schema")
            }
            Self::Conflict(_) => (
                "CONSTRAINT_CONFLICT",
                "Read the affected entities and correct the whole batch",
            ),
            Self::Version { .. } => (
                "VERSION_CONFLICT",
                "Read the current entity and retry with its version",
            ),
            Self::NotFound => (
                "NOT_FOUND",
                "Read context and use an entity from the authorized ledger",
            ),
            Self::Forbidden => ("FORBIDDEN", "Obtain the required OAuth scope"),
            Self::Idempotency => (
                "IDEMPOTENCY_CONFLICT",
                "Use the original request or a new key",
            ),
            Self::CursorStale => ("CURSOR_STALE", "Restart from the first page"),
            Self::Limit(_) => ("REQUEST_LIMIT", "Split the request into smaller batches"),
            Self::Overflow => (
                "AMOUNT_OVERFLOW",
                "Use integer amounts within NUMERIC(38,0) range",
            ),
            Self::Database(_) => (
                "DATABASE_UNAVAILABLE",
                "Retry with the same idempotency key",
            ),
        };
        let message = if matches!(self, Self::Database(_)) {
            tracing::error!(error = %self, "Database request failed");
            "Database operation failed".to_owned()
        } else {
            self.to_string()
        };
        json!(ErrorBody {
            code,
            message,
            next_actions: vec![action]
        })
    }
}

pub fn require(condition: bool, message: impl Into<String>) -> Result<()> {
    if condition {
        Ok(())
    } else {
        Err(Error::Invalid(message.into()))
    }
}

pub fn version(expected: i64, actual: i64) -> Result<()> {
    if expected == actual {
        Ok(())
    } else {
        Err(Error::Version { expected, actual })
    }
}
