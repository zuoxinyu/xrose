use std::path::Path;

/// Errors surfaced by the core. UI-facing messages are intentionally kept
/// human readable: the app shows `to_string()` directly.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),

    #[error("JSON error: {0}")]
    Json(#[from] serde_json::Error),

    #[error("YAML error: {0}")]
    Yaml(#[from] serde_yaml::Error),

    #[error("database error: {0}")]
    Database(#[from] rusqlite::Error),

    #[error("network error: {0}")]
    Http(#[from] reqwest::Error),

    #[error("configuration error: {0}")]
    Config(String),

    #[error("core binary error: {0}")]
    Binary(String),

    #[error("{name} 无法应用组网配置：{source}。请在设置中查看内核更新状态；更新后请重新应用代理配置", name = core.display_name())]
    MeshCore {
        core: crate::model::CoreKind,
        #[source]
        source: Box<Error>,
    },

    #[error("core process error: {0}")]
    Process(String),

    #[error("the selected core does not support: {0}")]
    Unsupported(String),

    #[error("subscription error: {0}")]
    Subscription(String),

    #[error("{0}")]
    Other(String),
}

impl Error {
    pub fn config(msg: impl Into<String>) -> Self {
        Self::Config(msg.into())
    }

    pub fn binary(msg: impl Into<String>) -> Self {
        Self::Binary(msg.into())
    }

    pub fn process(msg: impl Into<String>) -> Self {
        Self::Process(msg.into())
    }

    pub fn other(msg: impl Into<String>) -> Self {
        Self::Other(msg.into())
    }

    pub fn unsupported(msg: impl Into<String>) -> Self {
        Self::Unsupported(msg.into())
    }

    /// True when the failure was caused by a lack of filesystem write/access
    /// permissions or a read-only database directory.
    pub fn is_permission_or_readonly(&self) -> bool {
        match self {
            Self::Io(err) => {
                err.kind() == std::io::ErrorKind::PermissionDenied
                    || matches!(err.raw_os_error(), Some(5))
            }
            Self::Database(rusqlite::Error::SqliteFailure(err, _)) => matches!(
                err.code,
                rusqlite::ErrorCode::PermissionDenied
                    | rusqlite::ErrorCode::ReadOnly
                    | rusqlite::ErrorCode::CannotOpen
            ),
            Self::MeshCore { source, .. } => source.is_permission_or_readonly(),
            _ => false,
        }
    }
}

pub type Result<T> = std::result::Result<T, Error>;

/// Context-free I/O helper used around paths.
pub fn io_context(path: &std::path::Path, err: std::io::Error) -> Error {
    let hint = if err.kind() == std::io::ErrorKind::PermissionDenied {
        "（没有访问权限：请检查目录归属，或把 XROSE_HOME 指向可写目录）"
    } else {
        ""
    };
    Error::Io(std::io::Error::new(
        err.kind(),
        format!("{}: {err}{hint}", path.display()),
    ))
}

/// Convenience for callers that need to report a path in an otherwise opaque error.
pub fn path_display(path: &Path) -> String {
    path.display().to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detects_permission_and_readonly_errors() {
        let denied = io_context(
            Path::new("fixture"),
            std::io::Error::new(std::io::ErrorKind::PermissionDenied, "access denied"),
        );
        assert!(denied.is_permission_or_readonly());

        let not_found = io_context(
            Path::new("fixture"),
            std::io::Error::new(std::io::ErrorKind::NotFound, "missing"),
        );
        assert!(!not_found.is_permission_or_readonly());
    }
}
