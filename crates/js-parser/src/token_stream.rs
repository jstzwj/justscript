//! A trivia-filtering, backtracking-capable token stream.
//!
//! Wraps the lexer's [`Token`](js_syntax::Token) iterator and exposes the
//! primitives a recursive-descent parser wants: peek (N ahead), expect,
//! consume-if, and snapshot/restore for backtracking. Trivia tokens
//! (whitespace, line terminators, comments) are skipped.

use js_diagnostics::Diagnostic;
use js_syntax::source::Span;
use js_syntax::token::{Token, TokenKind};
use js_syntax::Keyword;
use js_lexer::tokenize;
use std::collections::VecDeque;

pub struct ParserTokenStream {
    /// Buffered non-trivia tokens. The buffer always contains at least the
    /// next token to be consumed, plus lookahead.
    buf: VecDeque<Token>,
    /// Underlying lexer output, drained lazily into `buf`.
    lexer: std::vec::IntoIter<Token>,
}

impl ParserTokenStream {
    pub fn new(src: &str) -> ParserTokenStream {
        let tokens: Vec<Token> = tokenize(src)
            .filter(|t| !t.kind.is_trivia())
            .collect();
        let mut stream = ParserTokenStream {
            buf: VecDeque::with_capacity(8),
            lexer: tokens.into_iter(),
        };
        stream.fill(3);
        stream
    }

    fn fill(&mut self, want: usize) {
        while self.buf.len() < want {
            match self.lexer.next() {
                Some(t) => self.buf.push_back(t),
                None => break,
            }
        }
    }

    /// The current (not yet consumed) token.
    pub fn peek(&mut self) -> &Token {
        self.fill(1);
        self.buf.front().expect("stream always has at least EOF")
    }

    /// Peek the kind of the current token (convenience).
    pub fn peek_kind(&mut self) -> &TokenKind {
        &self.peek().kind
    }

    /// The token *after* the current one.
    pub fn peek2(&mut self) -> &Token {
        self.fill(2);
        self.buf.get(1).unwrap_or_else(|| self.buf.front().unwrap())
    }

    pub fn is_eof(&mut self) -> bool {
        matches!(self.peek().kind, TokenKind::Eof)
    }

    /// Current token's span.
    pub fn span(&mut self) -> Span {
        self.peek().span
    }

    /// Consume and return the current token.
    pub fn bump(&mut self) -> Token {
        self.fill(1);
        self.buf
            .pop_front()
            .unwrap_or_else(|| Token::new(TokenKind::Eof, Span::DUMMY))
    }

    /// Consume the current token only if it matches `kind`.
    pub fn eat(&mut self, kind: &TokenKind) -> bool {
        if self.peek_kind() == kind {
            self.bump();
            true
        } else {
            false
        }
    }

    /// Consume the current token only if it is the given keyword.
    pub fn eat_keyword(&mut self, kw: Keyword) -> bool {
        if matches!(self.peek_kind(), TokenKind::Keyword(k) if *k == kw) {
            self.bump();
            true
        } else {
            false
        }
    }

    /// Consume the current token only if it is the given punctuator.
    pub fn eat_punctuator(&mut self, p: js_syntax::Punctuator) -> bool {
        if matches!(self.peek_kind(), TokenKind::Punctuator(pp) if *pp == p) {
            self.bump();
            true
        } else {
            false
        }
    }

    /// Expect `kind`; on mismatch, push a diagnostic and return `None`.
    pub fn expect(&mut self, kind: TokenKind) -> Result<Token, Diagnostic> {
        if self.peek_kind() == &kind {
            Ok(self.bump())
        } else {
            let span = self.span();
            Err(Diagnostic::error(
                span,
                format!("expected {:?}, found {:?}", kind, self.peek_kind()),
            ))
        }
    }

    /// Snapshot the stream position; pass the returned token to [`restore`](Self::restore).
    pub fn snapshot(&self) -> Vec<Token> {
        self.buf.iter().cloned().collect()
    }

    /// Restore to a snapshot taken with [`snapshot`](Self::snapshot). Tokens
    /// already drained from the lexer are *not* re-read — backtracking is only
    /// valid within the current lookahead window.
    pub fn restore(&mut self, snap: Vec<Token>) {
        self.buf = snap.into_iter().collect();
    }
}
