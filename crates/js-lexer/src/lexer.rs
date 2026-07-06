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
}

impl<'a> Lexer<'a> {
    pub fn new(src: &'a str) -> Lexer<'a> {
        Lexer {
            src,
            cursor: Cursor::new(src),
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
        Token::new(kind, Span::new(start, end))
    }

    fn advance_kind(&mut self, first: char) -> TokenKind {
        // Whitespace and line terminators.
        if is_whitespace(first) {
            self.cursor.eat_while(is_whitespace);
            return TokenKind::Whitespace;
        }
        if is_line_terminator(first) {
            self.cursor.bump();
            return TokenKind::LineTerminator;
        }

        // Comments.
        if first == '/' {
            match self.cursor.second() {
                '/' => {
                    self.cursor.eat_while(|c| !is_line_terminator(c));
                    return TokenKind::Comment { is_block: false };
                }
                '*' => {
                    self.cursor.bump(); // '/'
                    self.cursor.bump(); // '*'
                    return self.eat_block_comment();
                }
                _ => {}
            }
        }

        // Identifiers & keywords.
        if is_id_start(first) {
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
        while !self.cursor.is_eof() {
            let c = self.cursor.first();
            if c == '*' && self.cursor.second() == '/' {
                self.cursor.bump();
                self.cursor.bump();
                break;
            }
            self.cursor.bump();
        }
        TokenKind::Comment { is_block: true }
    }

    fn eat_identifier(&mut self) -> TokenKind {
        let start = self.cursor.byte_offset();
        // Simple path: consume the run of id-continue chars. (Unicode escapes
        // like `\u{...}` in identifiers are TODO.)
        self.cursor.eat_while(is_id_continue);
        let text = self.snippet_from(start);
        if let Ok(kw) = Keyword::from_str(&text) {
            TokenKind::Keyword(kw)
        } else {
            TokenKind::Ident(text)
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
            if c == EOF_CHAR || is_line_terminator(c) {
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

    /// Maximal-munch punctuator recognition. We try 2-char punctuators first,
    /// then fall back to single chars. (4-char `>>>=` etc. require a wider
    /// lookahead window — TODO.)
    fn eat_punctuator(&mut self) -> Option<Punctuator> {
        let c0 = self.cursor.first();
        let c1 = self.cursor.second();
        // Try the 2-char punctuator first.
        let two = [c0, c1];
        if let Some(p) = match_punctuator2(&two) {
            self.cursor.bump();
            self.cursor.bump();
            return Some(p);
        }
        // Fall back to a single char.
        match_punctuator1(c0).inspect(|_| {
            self.cursor.bump();
        })
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
    matches!(c, ' ' | '\t')
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

fn match_punctuator2(chars: &[char; 2]) -> Option<Punctuator> {
    use Punctuator::*;
    let s: String = chars.iter().collect();
    Some(match s.as_str() {
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
        _ => return None,
    })
}

fn match_punctuator1(c: char) -> Option<Punctuator> {
    use Punctuator::*;
    Some(match c {
        '{' => LBrace,
        '}' => RBrace,
        '(' => LParen,
        ')' => RParen,
        '[' => LBracket,
        ']' => RBracket,
        '.' => Dot,
        ';' => Semicolon,
        ',' => Comma,
        ':' => Colon,
        '?' => QuestionMark,
        '!' => Not,
        '~' => BitNot,
        '+' => Add,
        '-' => Sub,
        '*' => Mul,
        '/' => Div,
        '%' => Mod,
        '&' => BitAnd,
        '|' => BitOr,
        '^' => BitXor,
        '<' => Lt,
        '>' => Gt,
        '=' => Assign,
        _ => return None,
    })
}

// Silence unused import in tests-only builds.
#[allow(dead_code)]
const _EOF: char = EOF_CHAR;
