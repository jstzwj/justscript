use self::super::token::*;
use self::super::span::{BytePos, CharPos, Span, SourceFile};
use self::super::cursor::*;
use std::sync::Arc;
use std::str::Chars;
use std::error;
use std::fmt;

/// Parses the first token from the provided input string.
pub fn first_token(input: &str) -> Token {
    debug_assert!(!input.is_empty());
    Cursor::new(input).advance_token()
}

/// Creates an iterator that produces tokens from the input string.
pub fn tokenize(mut input: &str) -> impl Iterator<Item = Token> + '_ {
    std::iter::from_fn(move || {
        if input.is_empty() {
            return None;
        }
        let token = first_token(input);
        input = &input[token.len..];
        Some(token)
    })
}

fn is_whitespace(c: char) -> bool {
    // WhiteSpace
    match c {
        /* <TAB> */ '\u{0009}' |
        /* <VT> */  '\u{000B}' |
        /* <FF> */  '\u{000C}' |
        /* <SP> */  '\u{0020}' |
        /* <NBSP> */'\u{00A0}' |
        /*<ZWNBSP>*/'\u{FEFF}' |
        /* <USP> */ '\u{1680}' | '\u{2000}'..='\u{200B}' | '\u{202F}' | '\u{3000}'
        => {
            true
        }
        // default
        _ => {
            false
        }
    }
}

fn is_lineterminator(c: char) -> bool {
    // LineTerminator
    match c {
        /* LF */    '\u{000a}' |
        /* CR */    '\u{000d}' |
        /* LS */    '\u{2028}' |
        /* PS */    '\u{2029}'
        => {
            true
        },
        // default
        _ => {
            false
        }
    }
}

/// http://unicode.org/cldr/utility/list-unicodeset.jsp?a=[:ID_Start=Yes:]
/// True if `c` is valid as a first character of an identifier.
/// See [ecmascript language reference](https://www.ecma-international.org/ecma-262/10.0/index.html#prod-UnicodeIDStart) for
/// a formal definition of valid identifier name.
pub fn is_id_start(c: char) -> bool {
    // This is XID_Start OR '_' (which formally is not a XID_Start).
    // We also add fast-path for ascii idents
    ('a' <= c && c <= 'z')
        || ('A' <= c && c <= 'Z')
        || c == '_'
        || (c > '\x7f' && unicode_xid::UnicodeXID::is_xid_start(c))
}

// http://unicode.org/cldr/utility/list-unicodeset.jsp?a=[:ID_Continue=Yes:]
/// True if `c` is valid as a non-first character of an identifier.
/// See [ecmascript language reference](https://www.ecma-international.org/ecma-262/10.0/index.html#prod-UnicodeIDContinue) for
/// a formal definition of valid identifier name.
pub fn is_id_continue(c: char) -> bool {
    // This is exactly XID_Continue.
    // We also add fast-path for ascii idents
    ('a' <= c && c <= 'z')
        || ('A' <= c && c <= 'Z')
        || ('0' <= c && c <= '9')
        || c == '_'
        || (c > '\x7f' && unicode_xid::UnicodeXID::is_xid_continue(c))
}

impl Cursor<'_> {
    // fn lex(&mut self, code: &str) {}
    fn eat_str(iter: &mut Chars, s: &str) -> bool {
        true
    }
    /*
    fn eat_unicode_escape_sequence(&mut self) -> TokenKind {
        true
    }
    */

    fn eat_identifier_name(&mut self) -> TokenKind {
        TokenKind::IdentifierName
    }

    fn advance_token(&mut self) -> Token {
        let first_char = self.bump().unwrap();
        let token_kind: TokenKind = match first_char {
            // whitespace
            c if is_whitespace(c) => TokenKind::WhiteSpace,
            // LineTerminator
            c if is_lineterminator(c) => TokenKind::LineTerminator,
            '/' => match self.first() {
                '/' => TokenKind::SingleLineComment,
                '*' => TokenKind::MultiLineComment,
                _ => TokenKind::Unknown
            },
            // identifier name
            c if is_id_start(c) => self.eat_identifier_name(),
            // punctuator
            '{' => TokenKind::OpenBrace,
            '}' => TokenKind::CloseBrace,
            '(' => TokenKind::OpenParen,
            ')' => TokenKind::CloseParen,
            _ => TokenKind::Unknown,
        };
        Token::new(token_kind, self.len_consumed())
    }
}



pub struct StringReader {
    /// Initial position, read-only.
    start_pos: BytePos,
    /// The absolute offset within the source_map of the current character.
    pub pos: BytePos,
    /// Stop reading src at this index.
    end_src_index: usize,
    /// Source text to tokenize.
    src: Arc<String>,
}

impl StringReader {
    pub fn new(
        source_file: Arc<SourceFile>,
    ) -> Self {
        if source_file.src.is_none() {
            /*
            sess.span_diagnostic
                .bug(&format!("cannot lex `source_file` without source: {}", source_file.name));
            */
        }

        let src = (*source_file.src.as_ref().unwrap()).clone();

        StringReader {
            start_pos: source_file.start_pos,
            pos: source_file.start_pos,
            end_src_index: src.len(),
            src: Arc::new(src),
        }
    }
}