//! A trivia-filtering, backtracking-capable token stream with newline
//! tracking for Automatic Semicolon Insertion (ASI).
//!
//! Trivia (whitespace, comments) is skipped once at construction, but for each
//! surviving token we remember whether a **line terminator** appeared between it
//! and the previous token — that bit drives ASI in [`crate::stmt`] / [`crate::expr`].
//!
//! The stream is an index over a pre-materialized `Vec<Slot>` of non-trivia
//! tokens, so [`snapshot`]/[`restore`] (used for arrow-function and
//! regex/backtracking disambiguation) are O(1).

use js_diagnostics::Diagnostic;
use js_lexer::{tokenize, Lexer};
use js_syntax::keyword::Keyword;
use js_syntax::source::Span;
use js_syntax::token::{Token, TokenKind};

/// One buffered token plus whether a line terminator preceded it.
#[derive(Clone)]
pub(crate) struct Slot {
    pub(crate) token: Token,
    pub(crate) preceded_by_newline: bool,
}

/// The async/generator status of the enclosing function. Drives whether
/// contextual keywords `await` / `yield` may be used as identifiers: `await` is
/// reserved inside async functions (and module top level), `yield` inside
/// generators. Both flags are syntactically known before a body is parsed, so no
/// directive-prologue lookahead is needed.
#[derive(Clone, Copy, Default, Debug)]
pub struct FnCtx {
    pub is_async: bool,
    pub is_generator: bool,
    /// Whether this context is strict-mode (module / class body / `"use strict"`
    /// directive). Strict-mode-only reserved words (`let`, `static`, …) may not
    /// be binding identifiers here.
    pub is_strict: bool,
}

pub struct ParserTokenStream {
    /// The raw source, retained so an ambiguous `/` token can be re-lexed under
    /// the grammar goal the parser demands (see [`Self::reslash_div`] /
    /// [`Self::reslash_regex`]).
    src: String,
    /// All non-trivia tokens, each tagged with whether a line terminator
    /// preceded it.
    tokens: Vec<Slot>,
    /// Index of the current (not yet consumed) token.
    pos: usize,
    /// Stack of function contexts; the top is the currently-parsing function.
    /// The bottom frame is the top-level script/module context.
    ctx_stack: Vec<FnCtx>,
}

impl ParserTokenStream {
    pub fn new(src: &str) -> ParserTokenStream {
        let src = src.to_string();
        let mut tokens = Vec::new();
        let mut pending_newline = false;
        for tok in tokenize(&src) {
            if tok.kind.is_trivia() {
                if matches!(tok.kind, TokenKind::LineTerminator) {
                    pending_newline = true;
                } else if matches!(tok.kind, TokenKind::Comment { is_block: true, has_newline: true }) {
                    // A block comment containing a line terminator acts as one.
                    pending_newline = true;
                }
                continue;
            }
            let nl = std::mem::replace(&mut pending_newline, false);
            tokens.push(Slot {
                token: tok,
                preceded_by_newline: nl,
            });
        }
        // Sentinel EOF so peek at end-of-input still works without bounds checks.
        tokens.push(Slot {
            token: Token::new(TokenKind::Eof, Span::DUMMY),
            preceded_by_newline: pending_newline,
        });
        ParserTokenStream {
            src,
            tokens,
            pos: 0,
            ctx_stack: vec![FnCtx::default()],
        }
    }

    /// Mark the top-level context as a module (top-level `await` reserved;
    /// modules are always strict-mode).
    pub fn set_module(&mut self) {
        if let Some(top) = self.ctx_stack.first_mut() {
            top.is_async = true;
            top.is_strict = true;
        }
    }

    /// The current function context (top of the stack).
    pub fn current_ctx(&self) -> FnCtx {
        self.ctx_stack.last().copied().unwrap_or_default()
    }

    /// Push a new function context (entered a function/arrow/method body).
    pub fn push_ctx(&mut self, ctx: FnCtx) {
        self.ctx_stack.push(ctx);
    }

    /// Enter a function body, inheriting strict-ness from the enclosing context.
    pub fn enter_fn(&mut self, is_async: bool, is_generator: bool) {
        let is_strict = self.current_ctx().is_strict;
        self.ctx_stack.push(FnCtx { is_async, is_generator, is_strict });
    }

    /// Enter a class-member body (class elements are always strict-mode).
    pub fn enter_strict_fn(&mut self, is_async: bool, is_generator: bool) {
        self.ctx_stack.push(FnCtx { is_async, is_generator, is_strict: true });
    }

    /// Pop a function context (must match a prior `push_ctx`).
    pub fn pop_ctx(&mut self) {
        if self.ctx_stack.len() > 1 {
            self.ctx_stack.pop();
        }
    }

    /// The current (not yet consumed) token.
    pub fn peek(&self) -> &Token {
        &self.tokens[self.pos].token
    }

    /// Peek the kind of the current token (convenience).
    pub fn peek_kind(&self) -> &TokenKind {
        &self.tokens[self.pos].token.kind
    }

    /// Whether the **current** token was preceded by a line terminator — the key
    /// input to ASI and to postfix-update restricted productions.
    pub fn preceded_by_newline(&self) -> bool {
        self.tokens[self.pos].preceded_by_newline
    }

    /// Whether the token `n` positions ahead of the current one was preceded by
    /// a line terminator (`0` == current). Used for `[no LineTerminator here]`
    /// restricted productions like `let` disambiguation.
    pub(crate) fn preceded_by_newline_at(&self, n: usize) -> bool {
        let i = (self.pos + n).min(self.tokens.len() - 1);
        self.tokens[i].preceded_by_newline
    }

    /// The token *after* the current one.
    pub fn peek2(&self) -> &Token {
        let i = (self.pos + 1).min(self.tokens.len() - 1);
        &self.tokens[i].token
    }

    /// The token two after the current one (`peek3()`).
    pub fn peek3(&self) -> &Token {
        let i = (self.pos + 2).min(self.tokens.len() - 1);
        &self.tokens[i].token
    }

    /// Peek the token `n` positions ahead of the current one (`0` == current).
    pub fn peek_at(&self, n: usize) -> &Token {
        let i = (self.pos + n).min(self.tokens.len() - 1);
        &self.tokens[i].token
    }

    pub fn is_eof(&self) -> bool {
        matches!(self.tokens[self.pos].token.kind, TokenKind::Eof)
    }

    /// Current token's span.
    pub fn span(&self) -> Span {
        self.tokens[self.pos].token.span
    }

    /// Consume and return the current token.
    pub fn bump(&mut self) -> Token {
        let tok = self.tokens[self.pos].token.clone();
        if self.pos + 1 < self.tokens.len() {
            self.pos += 1;
        }
        tok
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

    /// Expect `kind`; on mismatch, push a diagnostic and return `Err`.
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

    /// Snapshot the stream *position*; pass it to [`restore`](Self::restore).
    /// Cheap (a single `usize`), enabling speculative parses (arrow functions).
    pub fn snapshot(&self) -> usize {
        self.pos
    }

    /// Restore to a position from [`snapshot`](Self::snapshot).
    pub fn restore(&mut self, snap: usize) {
        self.pos = snap;
    }

    // ---- regex / division disambiguation (parser-driven re-lex) ------------
    //
    // The lexer decides whether a `/` is a regex or the division operator from
    // the *previous* token (see `Lexer::update_regex_state`). That heuristic is
    // correct for almost every token — except `}` (which may close a block ⇒
    // regex follows, or a value-producing object/function expression ⇒ division
    // follows) and `)` (block-header close ⇒ regex follows, vs. value close ⇒
    // division). The lexer cannot tell these apart; the parser can.
    //
    // Rather than enumerate every value-`}` / header-`)` site (fragile), the
    // parser re-lexes an ambiguous `/` at the two grammar positions where its
    // intent is certain: at an *operand* start a `/` is always a regex, and at a
    // *binary-operator* position a `/` is always division. Both are provably
    // unambiguous (a division operator never begins an operand; a regex never
    // serves as a binary operator), so a mis-classified `/` at either point is
    // always the *other* goal. See `reslash_regex` / `reslash_div`.
    //
    // Both re-lex from the `/` *to end of input*: the lexer's only mistake is
    // this single goal decision, and its downstream `regex_allowed` state
    // (division ⇒ true, regex ⇒ false) diverges from there, so re-lexing the
    // whole tail under the corrected goal reproduces exactly the tokenization a
    // correct lexer would have produced. (Bounded re-lex would be wrong: a
    // mis-scanned regex can run past a string-literal boundary, and the
    // division interpretation of those same bytes may extend a string past the
    // original token's end.)

    /// Re-lex from byte offset `start` to end of input under the given
    /// regex/division goal, producing non-trivia slots. `first_newline` is the
    /// line-terminator context inherited by the first slot (preserving ASI).
    fn lex_tail(&self, start: usize, regex_allowed: bool, first_newline: bool) -> Vec<Slot> {
        let mut lexer = Lexer::new_at(&self.src, start, regex_allowed);
        let mut out = Vec::new();
        let mut pending_newline = first_newline;
        loop {
            let tok = lexer.advance_token();
            if matches!(tok.kind, TokenKind::Eof) {
                break;
            }
            if tok.kind.is_trivia() {
                if matches!(tok.kind, TokenKind::LineTerminator)
                    || matches!(tok.kind, TokenKind::Comment { is_block: true, has_newline: true })
                {
                    pending_newline = true;
                }
                continue;
            }
            let nl = std::mem::replace(&mut pending_newline, false);
            out.push(Slot { token: tok, preceded_by_newline: nl });
        }
        out
    }

    /// The current token is a `/` (division / division-assign) but the parser is
    /// at an operand start, so it must be a *regex literal*. Re-lex the tail from
    /// its start under the regex goal, splicing the corrected tokens in place of
    /// everything from the current position onward.
    pub(crate) fn reslash_regex(&mut self) {
        let start = self.tokens[self.pos].token.span.start.to_usize();
        let first_nl = self.tokens[self.pos].preceded_by_newline;
        let tail = self.lex_tail(start, true, first_nl);
        self.splice_tail(tail);
    }

    /// The current token is a regex literal (or an `Unknown('/')` produced when
    /// a regex scan ran past valid bounds) but the parser is at a binary-operator
    /// position, so the `/` must be *division*. Re-lex the tail from its start
    /// under the division goal, splicing the corrected tokens in.
    pub(crate) fn reslash_div(&mut self) {
        let start = self.tokens[self.pos].token.span.start.to_usize();
        let first_nl = self.tokens[self.pos].preceded_by_newline;
        let tail = self.lex_tail(start, false, first_nl);
        self.splice_tail(tail);
    }

    /// Replace every slot from the current position onward with `tail`, then
    /// re-append the EOF sentinel.
    fn splice_tail(&mut self, tail: Vec<Slot>) {
        let eof_nl = self.tokens.last().map_or(false, |s| s.preceded_by_newline);
        self.tokens.truncate(self.pos);
        self.tokens.extend(tail);
        self.tokens.push(Slot {
            token: Token::new(TokenKind::Eof, Span::DUMMY),
            preceded_by_newline: eof_nl,
        });
    }
}
