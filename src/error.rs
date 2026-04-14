// Copyright (c) 2026 Mike Strathmann
// SPDX-License-Identifier: MIT

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
#[path = "error_tests.rs"]
mod tests;
