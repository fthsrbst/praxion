//! Load errors: every rejection names its file (when known), line and column.

use std::fmt;

use crate::ast::{Pos, Span};

/// One load-time rejection.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Error {
    pub message: String,
    pub file: Option<String>,
    pub line: u32,
    pub col: u32,
}

impl Error {
    pub fn new(message: impl Into<String>, line: u32, col: u32) -> Self {
        Self {
            message: message.into(),
            file: None,
            line,
            col,
        }
    }

    pub fn at(message: impl Into<String>, pos: Pos) -> Self {
        Self::new(message, pos.line, pos.col)
    }

    pub fn span(message: impl Into<String>, span: Span) -> Self {
        Self::at(message, span.start)
    }

    pub fn with_file(mut self, file: impl Into<String>) -> Self {
        self.file = Some(file.into());
        self
    }

    pub fn pos(&self) -> Pos {
        Pos::new(self.line, self.col)
    }
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match &self.file {
            Some(file) => write!(f, "{file}:{}:{}: {}", self.line, self.col, self.message),
            None => write!(f, "{}:{}: {}", self.line, self.col, self.message),
        }
    }
}

impl std::error::Error for Error {}
