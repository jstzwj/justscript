//! The token-producing layer of the lexer.
//!
//! [`Lexer::advance_token`] inspects the current cursor position and produces
//! exactly one [`Token`] (trivia included). The public [`tokenize`] iterator
//! drives this until EOF.

use crate::cursor::{Cursor, EOF_CHAR};
use js_syntax::keyword::Keyword;
use js_syntax::punctuator::Punctuator;
use js_syntax::source::{BytePos, Span};
use js_syntax::token::{Token, TokenKind};
use std::str::FromStr;
use unicode_xid::UnicodeXID;

pub struct Lexer<'a> {
    src: &'a str,
    cursor: Cursor<'a>,
    /// Whether a leading `/` at the current position should be scanned as a
    /// regex literal rather than the division operator. Updated after each
    /// significant (non-trivia) token — see [`Self::update_regex_state`].
    regex_allowed: bool,
    /// Brace-nesting depth of each open template substitution `${ ... }`. When
    /// non-empty we are inside one or more substitutions; a `}` at depth 0 of
    /// the top frame closes the substitution and resumes template scanning.
    tmpl_stack: Vec<i32>,
    /// Set when a substitution just closed (`}` at depth 0): the next token to
    /// emit is the template *continuation* chunk (the text from here up to the
    /// next `` ` `` or `${`). Emitting the closing `}` as a regular `RBrace`
    /// and the chunk as a separate `Template` token keeps template-continuation
    /// tokens unambiguous with tagged-template literals (`a\`…\``).
    tmpl_pending: bool,
}

impl<'a> Lexer<'a> {
    pub fn new(src: &'a str) -> Lexer<'a> {
        Lexer {
            src,
            cursor: Cursor::new(src),
            regex_allowed: true,
            tmpl_stack: Vec::new(),
            tmpl_pending: false,
        }
    }

    /// A lexer positioned at `byte_offset`, with the given regex/division goal as
    /// its initial state. The parser uses this to *re-lex* an ambiguous `/` token
    /// under the goal demanded by its grammar position (operand start ⇒ regex,
    /// operator position ⇒ division) — fixing the cases the lexer's
    /// previous-token heuristic gets wrong (notably a `/` right after a `}` that
    /// closed a value-producing expression, or a `)` that closed a statement
    /// header). Template-substitution state is empty, which is correct: a
    /// re-lexed `/` range never straddles a template boundary.
    pub fn new_at(src: &'a str, byte_offset: usize, regex_allowed: bool) -> Lexer<'a> {
        Lexer {
            src,
            cursor: Cursor::new_at(src, byte_offset),
            regex_allowed,
            tmpl_stack: Vec::new(),
            tmpl_pending: false,
        }
    }

    /// Produce the next token, or `Eof` when input is exhausted.
    pub fn advance_token(&mut self) -> Token {
        let start = self.cursor.byte_offset();
        if self.cursor.is_eof() {
            return Token::new(TokenKind::Eof, Span::new(start, start));
        }
        let first = self.cursor.first();
        let kind = self.advance_kind(first);
        let end = self.cursor.byte_offset();
        // Maintain the regex/division disambiguation state across tokens.
        if !kind.is_trivia() && !matches!(kind, TokenKind::Eof) {
            self.update_regex_state(&kind);
        }
        Token::new(kind, Span::new(start, end))
    }

    /// After emitting a significant token, decide whether a following `/` is a
    /// regex. Regex is allowed unless the previous token plausibly *ends* an
    /// expression (identifier, literal, `)`, `]`, or `this`/`super`/literals).
    fn update_regex_state(&mut self, kind: &TokenKind) {
        self.regex_allowed = match kind {
            TokenKind::Ident(_)
            | TokenKind::Numeric(_)
            | TokenKind::String(_)
            | TokenKind::Bigint(_)
            | TokenKind::Regex { .. } => false,
            TokenKind::Keyword(k) => matches!(
                k,
                js_syntax::Keyword::This
                    | js_syntax::Keyword::Super
                    | js_syntax::Keyword::True
                    | js_syntax::Keyword::False
                    | js_syntax::Keyword::Null
                    | js_syntax::Keyword::Undefined
            ),
            // After a punctuator, regex is allowed unless the punctuator ends a
            // sub-expression (`)`, `]`); `}` is ambiguous (block vs. object
            // literal) and we default to allowing regex. Operators, `(`, `[`,
            // `{`, `,`, `;`, `=`, etc. all legitimately precede a regex.
            TokenKind::Punctuator(p) => !matches!(
                p,
                js_syntax::Punctuator::RParen | js_syntax::Punctuator::RBracket
            ),
            // `}` is ambiguous (block end vs. object literal); default to
            // allowing regex, the common case after a statement block.
            // A template head/middle chunk opens a substitution expression
            // (regex allowed inside it); a tail chunk ends the expression.
            TokenKind::Template { tail, .. } => !tail,
            TokenKind::PrivateName(_) => false,
            _ => true,
        };
        // Keyword(true/false/...) returned `false` above; all other keywords
        // fall through to `_ => true`. (The match arm already covers that.)
    }

    fn advance_kind(&mut self, first: char) -> TokenKind {
        // A template substitution just closed — the cursor sits at the start of
        // the next template chunk (text up to `` ` `` or `${`).
        if self.tmpl_pending {
            self.tmpl_pending = false;
            let (raw, cooked, tail) = self.scan_template_chunk();
            return TokenKind::Template { raw, cooked, tail };
        }
        // Hashbang comment: `#!...` is allowed only as the very first thing in
        // the source (a single line comment, like a shell shebang).
        if first == '#' && self.cursor.second() == '!' && self.cursor.byte_offset() == BytePos(0) {
            self.cursor.eat_while(|c| !is_line_terminator(c));
            return TokenKind::Comment { is_block: false, has_newline: false };
        }

        // Private name `#name` (class private fields/methods/accessors). `#`
        // followed by an identifier-start char — or a `\u` escape decoding to
        // one — starts a PrivateName; a bare `#` (e.g. `#!` mid-source) falls
        // through to Unknown.
        if first == '#' && (is_id_start(self.cursor.second()) || self.cursor.second() == '\\') {
            self.cursor.bump(); // '#'
            let mut name = String::new();
            match self.read_id_unit() {
                Some(c) if is_id_start(c) => name.push(c),
                _ => return TokenKind::Unknown('#'),
            }
            while let Some(c) = self.read_id_unit() {
                if is_id_continue(c) {
                    name.push(c);
                } else {
                    return TokenKind::Unknown('#');
                }
            }
            return TokenKind::PrivateName(name);
        }

        // Decorator `@`.
        if first == '@' {
            self.cursor.bump();
            return TokenKind::Punctuator(Punctuator::At);
        }

        // Whitespace and line terminators.
        if is_whitespace(first) {
            self.cursor.eat_while(is_whitespace);
            return TokenKind::Whitespace;
        }
        if is_line_terminator(first) {
            self.cursor.bump();
            return TokenKind::LineTerminator;
        }

        // Comments vs. regex vs. division — all start with `/`.
        if first == '/' {
            match self.cursor.second() {
                '/' => {
                    self.cursor.eat_while(|c| !is_line_terminator(c));
                    return TokenKind::Comment { is_block: false, has_newline: false };
                }
                '*' => {
                    self.cursor.bump(); // '/'
                    self.cursor.bump(); // '*'
                    return self.eat_block_comment();
                }
                _ => {}
            }
            // Not a comment: regex literal (when allowed) or division operator.
            if self.regex_allowed {
                return self.eat_regex();
            }
            // else: fall through to punctuator matching (Div / DivAssign).
        }

        // Template literals start with a backtick.
        if first == '`' {
            self.cursor.bump(); // opening backtick
            let (raw, cooked, tail) = self.scan_template_chunk();
            return TokenKind::Template { raw, cooked, tail };
        }

        // Inside a template substitution, `{`/`}` drive the substitution depth.
        if first == '{' && !self.tmpl_stack.is_empty() {
            if let Some(top) = self.tmpl_stack.last_mut() {
                *top += 1;
            }
            // Fall through to emit LBrace.
        }
        if first == '}' {
            if let Some(top) = self.tmpl_stack.last() {
                if *top == 0 {
                    // Close the substitution: emit the `}` as a regular RBrace
                    // (so it unambiguously terminates the substitution's
                    // expression), and resume template scanning on the NEXT
                    // token via `tmpl_pending`.
                    self.tmpl_stack.pop();
                    self.cursor.bump(); // consume the closing `}`
                    self.tmpl_pending = true;
                    return TokenKind::Punctuator(Punctuator::RBrace);
                }
            }
            if let Some(top) = self.tmpl_stack.last_mut() {
                *top -= 1;
            }
            // Fall through to emit RBrace.
        }

        // Identifiers & keywords (including those begun with a `\u` unicode escape).
        if is_id_start(first) || (first == '\\' && self.cursor.second() == 'u') {
            return self.eat_identifier();
        }
        // Numeric literals.
        if first.is_ascii_digit() || (first == '.' && self.cursor.second().is_ascii_digit()) {
            return self.eat_number();
        }
        // String literals.
        if first == '\'' || first == '"' {
            return self.eat_string(first);
        }

        // Punctuators (maximal munch).
        if let Some(p) = self.eat_punctuator() {
            return TokenKind::Punctuator(p);
        }

        // Unknown single char — surface as a diagnostic-friendly token.
        self.cursor.bump();
        TokenKind::Unknown(first)
    }

    fn eat_block_comment(&mut self) -> TokenKind {
        // Consume until `*/`. Unterminated comments surface as a single token.
        // Track whether a line terminator appears inside — per spec 12.4, a
        // block comment containing a line terminator acts as one for ASI.
        let mut has_newline = false;
        while !self.cursor.is_eof() {
            let c = self.cursor.first();
            if c == '*' && self.cursor.second() == '/' {
                self.cursor.bump();
                self.cursor.bump();
                break;
            }
            if is_line_terminator(c) {
                has_newline = true;
            }
            self.cursor.bump();
        }
        TokenKind::Comment { is_block: true, has_newline }
    }

    /// A regex literal `/pattern/flags`. Validates the RegularExpressionLiteral
    /// grammar: the first body char may not be `*`; a `\` must be followed by a
    /// non-line-terminator; the body may not contain a line terminator or be
    /// left unterminated; flags must be a subset of `gimsuvd` with no
    /// duplicates. On any violation, returns `Unknown('/')` so the parser
    /// surfaces a SyntaxError (the whole token's span covers the offense).
    fn eat_regex(&mut self) -> TokenKind {
        self.cursor.bump(); // opening '/'
        let mut pattern = String::new();
        let mut in_class = false;
        let mut first_body = true;
        let mut invalid = false;
        let mut paren_depth: i32 = 0;
        let mut defined_names: Vec<String> = Vec::new();
        let mut backrefs: Vec<String> = Vec::new();
        let mut saw_bare_k = false;
        loop {
            let c = self.cursor.first();
            if c == EOF_CHAR {
                invalid = true; // unterminated
                break;
            }
            if is_line_terminator(c) {
                invalid = true; // line terminator inside the body
                break;
            }
            if c == '\\' {
                self.cursor.bump(); // consume '\'
                let e = self.cursor.first();
                if e == EOF_CHAR || is_line_terminator(e) {
                    invalid = true; // incomplete backslash sequence
                    break;
                }
                if e == 'k' {
                    // Possible named backreference `\k<name>`.
                    self.cursor.bump(); // consume 'k'
                    if self.cursor.first() == '<' {
                        self.cursor.bump(); // consume '<'
                        match self.read_regex_name() {
                            Some(n) => {
                                backrefs.push(n.clone());
                                pattern.push_str(&format!("\\k<{}>", n));
                            }
                            None => {
                                invalid = true;
                                break;
                            }
                        }
                    } else {
                        // Bare `\k` (not `\k<`): a literal only if the pattern
                        // has no named groups; once named groups are in play it
                        // is a SyntaxError.
                        saw_bare_k = true;
                        pattern.push('\\');
                        pattern.push('k');
                    }
                    first_body = false;
                    continue;
                }
                pattern.push('\\');
                pattern.push(e);
                self.cursor.bump();
                first_body = false;
                continue;
            }
            if c == '[' {
                in_class = true;
                pattern.push(c);
                self.cursor.bump();
                first_body = false;
                continue;
            }
            if c == ']' {
                in_class = false;
                pattern.push(c);
                self.cursor.bump();
                continue;
            }
            if c == '/' && !in_class {
                self.cursor.bump();
                break;
            }
            if first_body && c == '*' {
                invalid = true; // `*` may not be the first body character
                break;
            }
            if c == '(' && !in_class {
                let ahead = self.cursor.peek_ahead4();
                if ahead[1] == '?' {
                    // `(?` must be followed by `:=!<` (rejects pattern-modifier
                    // proposal `(?i:…)`).
                    if !matches!(ahead[2], ':' | '=' | '!' | '<') {
                        invalid = true;
                        break;
                    }
                    if ahead[2] == '<' && !matches!(ahead[3], '=' | '!') {
                        // Named group `(?<name>`: consume `(?<`, read the name.
                        self.cursor.bump(); // (
                        self.cursor.bump(); // ?
                        self.cursor.bump(); // <
                        match self.read_regex_name() {
                            Some(n) => {
                                if defined_names.contains(&n) {
                                    invalid = true; // duplicate group name
                                    break;
                                }
                                defined_names.push(n.clone());
                                pattern.push_str(&format!("(?<{}>", n));
                            }
                            None => {
                                invalid = true; // empty / incomplete / invalid name
                                break;
                            }
                        }
                        paren_depth += 1;
                        first_body = false;
                        continue;
                    }
                }
                paren_depth += 1;
            }
            if c == ')' && !in_class {
                if paren_depth == 0 {
                    invalid = true; // unbalanced `)`
                    break;
                }
                paren_depth -= 1;
            }
            pattern.push(c);
            self.cursor.bump();
            first_body = false;
        }
        // Unbalanced `(` (group never closed) is a SyntaxError.
        if paren_depth != 0 {
            invalid = true;
        }
        // Named-group backreferences must resolve to a defined group.
        let has_named = !defined_names.is_empty() || !backrefs.is_empty();
        if has_named && saw_bare_k {
            invalid = true; // `\k` literal illegal once named groups are in play
        }
        if !invalid {
            for r in &backrefs {
                if !defined_names.contains(r) {
                    invalid = true; // dangling backreference
                    break;
                }
            }
        }
        // Flags: a run of identifier-continue chars. Validate the allowed set
        // and reject duplicates. Consume the full run either way so the trailing
        // chars don't re-lex as a stray identifier.
        let mut flags = String::new();
        let mut seen: u32 = 0;
        while is_id_continue(self.cursor.first()) {
            let f = self.cursor.first();
            flags.push(f);
            let bit = match f {
                'g' => 1 << 0,
                'i' => 1 << 1,
                'm' => 1 << 2,
                's' => 1 << 3,
                'u' => 1 << 4,
                'y' => 1 << 5,
                'v' => 1 << 6,
                'd' => 1 << 7,
                _ => 0,
            };
            if bit == 0 || (seen & bit) != 0 {
                invalid = true;
            }
            seen |= bit;
            self.cursor.bump();
        }
        if invalid {
            TokenKind::Unknown('/')
        } else {
            TokenKind::Regex { pattern, flags }
        }
    }


    /// Read a `RegExpIdentifierName` (used in `(?<name>` and `\k<name>`) starting
    /// at the current cursor, then consume the closing `>`. Returns `None` for
    /// an empty name, an invalid name char, or a missing `>`.
    fn read_regex_name(&mut self) -> Option<String> {
        let first = self.cursor.first();
        if !is_id_start(first) || first == EOF_CHAR || is_line_terminator(first) {
            return None; // empty name (`>`) or invalid start
        }
        let mut name = String::new();
        name.push(first);
        self.cursor.bump();
        while is_id_continue(self.cursor.first())
            && self.cursor.first() != EOF_CHAR
            && !is_line_terminator(self.cursor.first())
        {
            name.push(self.cursor.first());
            self.cursor.bump();
        }
        if self.cursor.first() != '>' {
            return None; // name never terminated
        }
        self.cursor.bump(); // consume '>'
        Some(name)
    }

    /// Scan one template chunk starting at the current cursor position (just
    /// after an opening backtick, or just after a substitution's closing `}`).
    /// Stops at a closing backtick (→ `tail` true) or at `${` (→ pushes a
    /// substitution frame, `tail` false). Returns `(raw, cooked, tail)`.
    fn scan_template_chunk(&mut self) -> (String, Option<String>, bool) {
        let mut raw = String::new();
        let mut cooked = String::new();
        let mut cooked_ok = true;
        loop {
            let c = self.cursor.first();
            if c == EOF_CHAR {
                // Unterminated template — treat what we have as a tail.
                return (raw, if cooked_ok { Some(cooked) } else { None }, true);
            }
            if c == '`' {
                self.cursor.bump();
                return (raw, if cooked_ok { Some(cooked) } else { None }, true);
            }
            if c == '$' && self.cursor.second() == '{' {
                self.cursor.bump();
                self.cursor.bump();
                self.tmpl_stack.push(0);
                return (raw, if cooked_ok { Some(cooked) } else { None }, false);
            }
            if c == '\\' {
                raw.push('\\');
                self.cursor.bump();
                let e = self.cursor.first();
                raw.push(e);
                if e == EOF_CHAR {
                    cooked_ok = false;
                    self.cursor.bump();
                    continue;
                }
                self.cursor.bump();
                match e {
                    'n' => cooked.push('\n'),
                    't' => cooked.push('\t'),
                    'r' => cooked.push('\r'),
                    'b' => cooked.push('\u{0008}'),
                    'f' => cooked.push('\u{000C}'),
                    'v' => cooked.push('\u{000B}'),
                    '0' => cooked.push('\0'),
                    '`' | '\\' | '$' => cooked.push(e),
                    // LineContinuation: `\` + line terminator → nothing in cooked.
                    lc if is_line_terminator(lc) => {}
                    other => cooked.push(other),
                }
                continue;
            }
            // CR/LF normalization in cooked (per spec, <CR><LF> and <CR> → <LF>).
            if c == '\r' {
                raw.push(c);
                cooked.push('\n');
                self.cursor.bump();
                if self.cursor.first() == '\n' {
                    raw.push('\n');
                    self.cursor.bump();
                }
                continue;
            }
            raw.push(c);
            cooked.push(c);
            self.cursor.bump();
        }
    }

    /// An identifier / keyword name. Identifier parts may be plain
    /// id-continue characters or unicode escapes (`\uXXXX`, `\u{...}`); the
    /// escapes are decoded to build the *cooked* identifier text, which is then
    /// checked against the keyword table. An escaped reserved word therefore
    /// produces the same `Keyword` token as its literal spelling — which is
    /// exactly right, because the parser already accepts keywords in
    /// IdentifierName contexts (property/member names) and rejects them in
    /// binding contexts.
    fn eat_identifier(&mut self) -> TokenKind {
        let mut text = String::new();

        // First unit must satisfy id_start.
        match self.read_id_unit() {
            Some(c) if is_id_start(c) => text.push(c),
            _ => return TokenKind::Unknown('\\'),
        }
        // Subsequent units must satisfy id_continue.
        while let Some(c) = self.read_id_unit() {
            if is_id_continue(c) {
                text.push(c);
            } else {
                return TokenKind::Unknown('\\');
            }
        }

        if let Ok(kw) = Keyword::from_str(&text) {
            TokenKind::Keyword(kw)
        } else {
            TokenKind::Ident(text)
        }
    }

    /// Consume one identifier unit from the cursor: either a plain character or
    /// a decoded `\uXXXX` / `\u{...}` escape (cursor is on the `\`). Returns
    /// `None` when the cursor is not on an identifier unit at all (so the
    /// caller can stop). On a malformed escape, the partial escape is consumed
    /// and `Some('\0')` is returned (a unit that fails id_start/id_continue),
    /// which the caller treats as an error.
    fn read_id_unit(&mut self) -> Option<char> {
        if self.cursor.first() == '\\' {
            return Some(self.read_unicode_escape().unwrap_or('\0'));
        }
        let c = self.cursor.first();
        if c == EOF_CHAR {
            return None;
        }
        // A non-id char ends the identifier.
        if !is_id_continue(c) && !is_id_start(c) {
            return None;
        }
        self.cursor.bump();
        Some(c)
    }

    /// Decode a `\uXXXX` or `\u{...}` escape with the cursor on `\`. Returns
    /// the decoded char, or `None` if malformed (the consumed prefix is not
    /// rolled back — callers only invoke this when a `\u` escape is expected).
    fn read_unicode_escape(&mut self) -> Option<char> {
        if self.cursor.first() != '\\' || self.cursor.second() != 'u' {
            return None;
        }
        self.cursor.bump(); // '\'
        self.cursor.bump(); // 'u'
        if self.cursor.first() == '{' {
            self.cursor.bump(); // '{'
            let mut value: u32 = 0;
            let mut count = 0;
            while let Some(d) = self.cursor.first().to_digit(16) {
                value = value.checked_mul(16)?.checked_add(d)?;
                self.cursor.bump();
                count += 1;
                if count > 6 {
                    return None;
                }
            }
            if self.cursor.first() != '}' || count == 0 {
                return None;
            }
            self.cursor.bump(); // '}'
            char::from_u32(value)
        } else {
            // Exactly four hex digits.
            let mut value: u32 = 0;
            for _ in 0..4 {
                let d = self.cursor.first().to_digit(16)?;
                value = value * 16 + d;
                self.cursor.bump();
            }
            char::from_u32(value)
        }
    }

    fn eat_number(&mut self) -> TokenKind {
        let start = self.cursor.byte_offset();
        // Radix prefixes.
        if self.cursor.first() == '0' {
            match self.cursor.second() {
                'x' | 'X' => {
                    self.cursor.bump();
                    self.cursor.bump();
                    self.cursor.eat_while(|c| c.is_ascii_hexdigit() || c == '_');
                    return self.numeric_or_bigint(start);
                }
                'b' | 'B' => {
                    self.cursor.bump();
                    self.cursor.bump();
                    self.cursor.eat_while(|c| c == '0' || c == '1' || c == '_');
                    return self.numeric_or_bigint(start);
                }
                'o' | 'O' => {
                    self.cursor.bump();
                    self.cursor.bump();
                    self.cursor.eat_while(|c| ('0'..='7').contains(&c) || c == '_');
                    return self.numeric_or_bigint(start);
                }
                _ => {}
            }
        }
        // Decimal integer part.
        self.cursor.eat_while(|c| c.is_ascii_digit() || c == '_');
        // Fractional part.
        if self.cursor.first() == '.' {
            self.cursor.bump();
            self.cursor.eat_while(|c| c.is_ascii_digit() || c == '_');
        }
        // Exponent.
        let e = self.cursor.first();
        if e == 'e' || e == 'E' {
            self.cursor.bump();
            if matches!(self.cursor.first(), '+' | '-') {
                self.cursor.bump();
            }
            self.cursor.eat_while(|c| c.is_ascii_digit() || c == '_');
        }
        self.numeric_or_bigint(start)
    }

    fn numeric_or_bigint(&mut self, start: BytePos) -> TokenKind {
        if self.cursor.first() == 'n' {
            self.cursor.bump();
            TokenKind::Bigint(self.snippet_from(start))
        } else {
            TokenKind::Numeric(self.snippet_from(start))
        }
    }

    fn eat_string(&mut self, quote: char) -> TokenKind {
        self.cursor.bump(); // opening quote
        let mut out = String::new();
        loop {
            let c = self.cursor.first();
            // Only `<LF>` (`\n`) / `<CR>` (`\r`) terminate a string literal
            // raw; ES2019 permits `<LS>` (U+2028) / `<PS>` (U+2029) inline.
            if c == EOF_CHAR || c == '\n' || c == '\r' {
                // Unterminated string — bail with what we have.
                break;
            }
            if c == quote {
                self.cursor.bump();
                break;
            }
            if c == '\\' {
                self.cursor.bump(); // backslash
                let esc = self.cursor.first();
                // LineContinuation: `\` + LineTerminatorSequence → nothing.
                if is_line_terminator(esc) {
                    self.cursor.bump();
                    // Collapse a `<CR><LF>` pair into one continuation.
                    if esc == '\r' && self.cursor.first() == '\n' {
                        self.cursor.bump();
                    }
                    continue;
                }
                self.cursor.bump();
                match esc {
                    'n' => out.push('\n'),
                    't' => out.push('\t'),
                    'r' => out.push('\r'),
                    '0' => out.push('\0'),
                    'b' => out.push('\u{0008}'),
                    'f' => out.push('\u{000C}'),
                    'v' => out.push('\u{000B}'),
                    '\\' | '\'' | '"' => out.push(esc),
                    '/' => out.push('/'),
                    'x' => {
                        if let Some(ch) = self.take_hex(2).and_then(char::from_u32) {
                            out.push(ch);
                        }
                    }
                    'u' => {
                        let hex = if self.cursor.first() == '{' {
                            self.cursor.bump();
                            self.take_hex_until_brace()
                        } else {
                            self.take_hex(4)
                        };
                        if let Some(ch) = hex.and_then(char::from_u32) {
                            out.push(ch);
                        }
                    }
                    EOF_CHAR => break,
                    other => out.push(other),
                }
                continue;
            }
            out.push(c);
            self.cursor.bump();
        }
        TokenKind::String(out)
    }

    /// Consume exactly `n` hex digits and return the parsed value.
    fn take_hex(&mut self, n: usize) -> Option<u32> {
        let mut acc = 0u32;
        for _ in 0..n {
            let c = self.cursor.first();
            let d = c.to_digit(16)?;
            self.cursor.bump();
            acc = acc * 16 + d;
        }
        Some(acc)
    }

    /// Consume hex digits until a closing `}` (for `\u{...}`).
    fn take_hex_until_brace(&mut self) -> Option<u32> {
        let mut acc = 0u32;
        loop {
            let c = self.cursor.first();
            if c == '}' {
                self.cursor.bump();
                return Some(acc);
            }
            if c == EOF_CHAR {
                return None;
            }
            let d = c.to_digit(16)?;
            self.cursor.bump();
            acc = acc.checked_mul(16)?.checked_add(d)?;
        }
    }

    /// Maximal-munch punctuator recognition. ECMAScript punctuators range from
    /// 1 to 4 characters (`>>>=`), so we read up to 4 upcoming chars and try the
    /// longest spelling first, bumping exactly the matched length.
    fn eat_punctuator(&mut self) -> Option<Punctuator> {
        let ahead = self.cursor.peek_ahead4();
        for len in (1..=4).rev() {
            let cand: String = ahead[..len].iter().collect();
            if let Some(p) = match_punctuator(&cand) {
                for _ in 0..len {
                    self.cursor.bump();
                }
                return Some(p);
            }
        }
        None
    }

    /// Slice `[start, current)` out of the source.
    fn snippet_from(&self, start: BytePos) -> String {
        let s = start.to_usize();
        let e = self.cursor.byte_offset().to_usize();
        self.src.get(s..e).map(|s| s.to_string()).unwrap_or_default()
    }
}

/// An iterator over all tokens (including trivia) of a source string.
pub struct Tokens<'a> {
    lexer: Lexer<'a>,
    done: bool,
}

impl<'a> Iterator for Tokens<'a> {
    type Item = Token;
    fn next(&mut self) -> Option<Token> {
        if self.done {
            return None;
        }
        let t = self.lexer.advance_token();
        if matches!(t.kind, TokenKind::Eof) {
            self.done = true;
        }
        Some(t)
    }
}

/// Tokenize `src`, returning an iterator of [`Token`]s (trivia included).
pub fn tokenize(src: &str) -> Tokens<'_> {
    Tokens {
        lexer: Lexer::new(src),
        done: false,
    }
}

// ---- helpers --------------------------------------------------------------

fn is_whitespace(c: char) -> bool {
    matches!(c, ' ' | '\t' | '\u{000B}' | '\u{000C}')
        || (c >= '\u{2000}' && c <= '\u{200A}')
        || matches!(c, '\u{00A0}' | '\u{FEFF}' | '\u{3000}')
}

fn is_line_terminator(c: char) -> bool {
    matches!(c, '\n' | '\r' | '\u{2028}' | '\u{2029}')
}

fn is_id_start(c: char) -> bool {
    c == '_' || c == '$' || UnicodeXID::is_xid_start(c)
}

fn is_id_continue(c: char) -> bool {
    c == '_' || c == '$' || UnicodeXID::is_xid_continue(c)
}

/// Resolve a candidate punctuator spelling (1–4 chars) to its [`Punctuator`].
/// Called by [`Lexer::eat_punctuator`] longest-first so maximal munch holds.
fn match_punctuator(s: &str) -> Option<Punctuator> {
    use Punctuator::*;
    Some(match s {
        // 4-char
        ">>>=" => UshrAssign,
        // 3-char
        "===" => StrictEq,
        "!==" => StrictNotEq,
        ">>>" => Ushr,
        "**=" => ExpAssign,
        "<<=" => ShlAssign,
        ">>=" => ShrAssign,
        "..." => Spread,
        "&&=" => AndAssign,
        "||=" => OrAssign,
        "??=" => NullishAssign,
        // 2-char
        "=>" => Arrow,
        "==" => Eq,
        "!=" => NotEq,
        "<=" => Le,
        ">=" => Ge,
        "&&" => And,
        "||" => Or,
        "??" => NullishCoal,
        "?." => OptChain,
        "**" => Exp,
        "<<" => Shl,
        ">>" => Shr,
        "+=" => AddAssign,
        "-=" => SubAssign,
        "*=" => MulAssign,
        "/=" => DivAssign,
        "%=" => ModAssign,
        "&=" => BitAndAssign,
        "|=" => BitOrAssign,
        "^=" => BitXorAssign,
        "++" => Inc,
        "--" => Dec,
        // 1-char
        "{" => LBrace,
        "}" => RBrace,
        "(" => LParen,
        ")" => RParen,
        "[" => LBracket,
        "]" => RBracket,
        "." => Dot,
        ";" => Semicolon,
        "," => Comma,
        ":" => Colon,
        "?" => QuestionMark,
        "!" => Not,
        "~" => BitNot,
        "+" => Add,
        "-" => Sub,
        "*" => Mul,
        "/" => Div,
        "%" => Mod,
        "&" => BitAnd,
        "|" => BitOr,
        "^" => BitXor,
        "<" => Lt,
        ">" => Gt,
        "=" => Assign,
        _ => return None,
    })
}

// Silence unused import in tests-only builds.
#[allow(dead_code)]
const _EOF: char = EOF_CHAR;

#[cfg(test)]
mod regex_tests {
    use super::*;

    /// Collect the non-trivia token kinds from `src`.
    fn kinds(src: &str) -> Vec<TokenKind> {
        tokenize(src)
            .filter(|t| !t.kind.is_trivia() && !matches!(t.kind, TokenKind::Eof))
            .map(|t| t.kind)
            .collect()
    }

    fn is_regex(src: &str) -> bool {
        kinds(src).into_iter().any(|k| matches!(k, TokenKind::Regex { .. }))
    }

    fn is_unknown_slash(src: &str) -> bool {
        // An invalid regex lexes as Unknown('/').
        kinds(src).into_iter().any(|k| matches!(k, TokenKind::Unknown('/')))
    }

    #[test]
    fn regex_after_operator() {
        // The core fix: `/` lexes as a regex after `=`, `(`, `,`, `;`, etc.
        assert!(is_regex("var x = /abc/"));
        assert!(is_regex("f(/re/)"));
        assert!(is_regex("x = /a/, y = /b/"));
    }

    #[test]
    fn division_still_works() {
        // `/` after an identifier / `)` / `]` is division, not a regex.
        assert!(!is_regex("a / b"));
        assert!(!is_regex("f() / 2"));
        assert!(!is_regex("a[0] / 2"));
    }

    #[test]
    fn valid_regex_lexes() {
        assert!(is_regex("/abc/"));
        assert!(is_regex("/a*/"));
        assert!(is_regex("/[abc]/"));
        assert!(is_regex("/\\d+/"));
        assert!(is_regex("/^a$/"));
        assert!(is_regex("/foo/gim"));
        assert!(is_regex("/(?=a)/"));
        assert!(is_regex("/(?<=a)b/"));
        assert!(is_regex("/(?<name>x)/"));
    }

    #[test]
    fn invalid_regex_rejected() {
        // Pattern-modifier proposal `(?i:…)` — invalid in base ES.
        assert!(is_unknown_slash("/(?i:a)/"));
        // Incomplete backslash sequence.
        assert!(is_unknown_slash("var x = /a\\/"));
        // Bad / duplicate flags.
        assert!(is_unknown_slash("var x = /a/q"));
        assert!(is_unknown_slash("var x = /a/gg"));
        // Unbalanced parens.
        assert!(is_unknown_slash("var x = /(a/"));
        assert!(is_unknown_slash("var x = /a)/"));
    }

    #[test]
    fn valid_named_groups() {
        assert!(is_regex("var x = /(?<year>\\d{4})/"));
        assert!(is_regex("var x = /(?<a>x)\\k<a>/"));
    }

    #[test]
    fn empty_group_name_rejected() {
        assert!(is_unknown_slash("var x = /(?<>a)/"));
    }

    #[test]
    fn duplicate_group_name_rejected() {
        assert!(is_unknown_slash("var x = /(?<a>a)(?<a>a)/"));
    }

    #[test]
    fn dangling_backref_rejected() {
        assert!(is_unknown_slash("var x = /(?<a>.)\\k<b>/"));
    }

    #[test]
    fn incomplete_group_name_rejected() {
        assert!(is_unknown_slash("var x = /(?<abc/"));
    }
}
