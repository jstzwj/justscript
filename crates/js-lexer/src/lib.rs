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

pub use cursor::Cursor;
pub use lexer::{tokenize, Lexer};

#[cfg(test)]
mod tests;
