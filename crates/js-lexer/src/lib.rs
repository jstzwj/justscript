//! The JustScript lexer.
//!
//! Two layers:
//! - [`cursor`] — a byte/char cursor over the raw source, providing
//!   `peek`/`bump` primitives (modeled after `rustc_lexer::Cursor`),
//! - [`lexer`] — `advance_token`, which turns the current cursor position into
//!   one [`Token`].
//!
//! The public entry point is [`tokenize`], an iterator over tokens (including
//! trivia). The parser filters trivia as it consumes.

pub mod cursor;
pub mod lexer;
mod unicode_id;
pub mod validate;

pub use cursor::Cursor;
pub use lexer::{tokenize, Lexer};
pub use validate::{parse_number, validate_numeric_literal, NumericError};

/// Unicode property version used for ECMAScript IdentifierName tokenization.
pub const IDENTIFIER_UNICODE_VERSION: (u8, u8, u8) = unicode_id::UNICODE_VERSION;

#[cfg(test)]
mod tests;
