//! Foundational syntax types shared across the JustScript toolchain.
//!
//! This crate has **no engine logic** and depends on nothing external. It owns
//! the single source of truth for:
//! - source positions ([`source`]),
//! - lexical tokens ([`token`]), keywords ([`keyword`]) and punctuators ([`punctuator`]),
//! - the abstract syntax tree ([`ast`]).

pub mod ast;
pub mod keyword;
pub mod punctuator;
pub mod source;
pub mod token;

pub use ast::*;
pub use keyword::Keyword;
pub use punctuator::Punctuator;
pub use source::{BytePos, Loc, SourceFile, SourceId, SourceMap, Span};
pub use token::{Token, TokenKind};
