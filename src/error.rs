use std::error::Error;
use std::fmt::{Display, Formatter};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CoreFsError {
    AlreadyExists(String),
    InvalidCommand(String),
    InvalidInput(String),
    NotFound(String),
    PolicyViolation(String),
    State(String),
}

impl Display for CoreFsError {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::AlreadyExists(message)
            | Self::InvalidCommand(message)
            | Self::InvalidInput(message)
            | Self::NotFound(message)
            | Self::PolicyViolation(message)
            | Self::State(message) => f.write_str(message),
        }
    }
}

impl Error for CoreFsError {}

pub type CoreFsResult<T> = Result<T, CoreFsError>;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn display_returns_inner_message_for_each_error_variant() {
        let variants = [
            CoreFsError::AlreadyExists("exists".to_string()),
            CoreFsError::InvalidCommand("command".to_string()),
            CoreFsError::InvalidInput("input".to_string()),
            CoreFsError::NotFound("missing".to_string()),
            CoreFsError::PolicyViolation("policy".to_string()),
            CoreFsError::State("state".to_string()),
        ];

        let rendered: Vec<String> = variants.iter().map(ToString::to_string).collect();
        assert_eq!(
            rendered,
            vec!["exists", "command", "input", "missing", "policy", "state"]
        );
    }
}
