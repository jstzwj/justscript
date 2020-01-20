mod cursor;
pub mod token;

use crate::syntax::token::TokenKind;

use token::LexToken;
use cursor::Cursor;

/// Parses the first token from the provided input string.
pub fn first_token(input: &str) -> LexToken {
    debug_assert!(!input.is_empty());
    Cursor::new(input).advance_token()
}

/// Creates an iterator that produces tokens from the input string.
pub fn tokenize(mut input: &str) -> impl Iterator<Item = LexToken> + '_ {
    std::iter::from_fn(move || {
        if input.is_empty() {
            return None;
        }
        let token = first_token(input);
        input = &input[token.len..];
        Some(token)
    })
}

fn hex2int(c:&char) -> i32 {
    match c {
        '0' => 0,
        '1' => 1,
        '2' => 2,
        '3' => 3,
        '4' => 4,
        '5' => 5,
        '6' => 6,
        '7' => 7,
        '8' => 8,
        '9' => 9,
        'a'|'A' => 10,
        'b'|'B' => 11,
        'c'|'C' => 12,
        'd'|'D' => 13,
        'e'|'E' => 14,
        'f'|'F' => 15,
        _ => 0
    }
}

fn is_hex_digit(c: &char) -> bool {
    match c {
        '0'..='9' | 'a'..='z' | 'A'..='Z' => true,
        _ => false,
    }
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
    fn eat_hex_digit(&mut self) -> bool {
        let c = self.first();
        match c {
            '0'..='9' | 'a'..='z' | 'A'..='Z' => true,
            _ => false
        }
    }
    fn eat_unicode_escape_sequence(&mut self) -> bool {
        let mut ret = true;
        let u = self.first();
        if u != 'u' {
            ret = false;
        } else {
            self.bump();
            if self.first() == '{' {
                self.bump();
                let mut num = 0x0;
                for _i in 0..6 {
                    if let Some(c) = self.bump() {
                        if is_hex_digit(&c) {
                            ret = false;
                            break;
                        } else {
                            num *= 16;
                            num += hex2int(&c);
                        }
                    } else {
                        ret = false;
                        break;
                    }
                }

                if num > 0x10FFFF {
                    ret = false;
                }

                if self.first() != '}' {
                    ret = false;
                    self.bump();
                }
            } else {
                for _i in 0..4 {
                    let c = self.first();
                    if !is_hex_digit(&c) {
                        ret = false;
                        break;
                    }
                    self.bump();
                }
            }
        }
        ret
    }

    fn eat_decimal_integer_literal(&mut self) -> bool {
        self.eat_while(|c| ('0'..='9').contains(&c));
        true
    }

    fn eat_decimal_digits(&mut self) -> bool {
        self.eat_while(|c| ('0'..='9').contains(&c));
        true
    }

    fn eat_octal_digits(&mut self) -> bool {
        self.eat_while(|c| ('0'..='7').contains(&c));
        true
    }

    fn eat_binary_digits(&mut self) -> bool {
        self.eat_while(|c| ('0'..='1').contains(&c));
        true
    }

    fn eat_hex_digits(&mut self) -> bool {
        self.eat_while(|c| match c{
            '0'..='9' | 'a'..='z' | 'A'..='Z' => true,
            _ => false
            }
        );
        true
    }

    fn eat_escape_sequence(&mut self) -> bool {
        match self.first() {
            // CharacterEscapeSequence
            '\''| '"' | '\\' | 'b' | 'f' | 'n' | 'r' | 't' | 'v' => {
                self.bump();
            },
            // 0
            '0' => {
                self.bump();
                match self.first() {
                    '0' ..= '9' => { self.eat_decimal_integer_literal(); },
                    _ => ()
                };
            },
            // HexEscapeSequence
            'x' => {
                self.bump();
                self.eat_hex_digits();
            },
            // UnicodeEscapeSequence
            'u' => {
                self.eat_unicode_escape_sequence();
            },
            // CharacterEscapeSequence - NonEscapeCharacter
            _ => (),
        };
        true
    }

    fn eat_exponent_part(&mut self) -> bool {
        match self.first() {
            '0'..='9' => {
                self.eat_decimal_digits()
            },
            '+' => {
                self.bump();
                self.eat_decimal_digits()
            },
            '-' => {
                self.bump();
                self.eat_decimal_digits()
            },
            _ => {
                false
            }
        }
    }

    fn eat_line_terminator_sequence(&mut self) -> bool {
        match self.bump() {
            Some(c) => match c{
                // <LF> <LS> <PS>
                '\u{000a}' | '\u{2028}' | '\u{2029}' => true,
                // <CR>
                '\u{000d}' => match self.first() {
                    // <LF>
                    '\u{000a}' => {
                        self.bump();
                        true
                    },
                    _ => true,
                },
                // other
                _ => false
            },
            None => false
        }
    }

    fn identifier_name(&mut self) -> TokenKind {
        while !self.is_eof() {
            let c = self.first();

            match c {
                c if is_id_continue(c) => {
                    self.bump();
                },
                '$' | '\u{200C}' | '\u{200D}' => {
                    self.bump();
                },
                '\\' => {
                    self.eat_unicode_escape_sequence();
                },
                _ => {
                    break;
                }
            };
            
        }
        TokenKind::Ident
    }

    fn single_line_comment(&mut self) -> TokenKind {
        self.bump();
        self.eat_while(|c| !is_lineterminator(c));
        TokenKind::SingleLineComment
    }

    fn multi_line_comment(&mut self) -> TokenKind {
        self.bump();
        while let Some(c) = self.bump() {
            match c {
                '*' if self.first() == '/' => {
                    self.bump();
                    break;
                }
                _ => (),
            }
        }

        TokenKind::MultiLineComment
    }

    fn binary_integer_literal(&mut self) -> TokenKind {
        if self.eat_binary_digits() {
            TokenKind::BinaryIntegerLiteral
        }
        else {
            TokenKind::Unknown
        }
    }

    fn octal_integer_literal(&mut self) -> TokenKind {
        if self.eat_octal_digits() {
            TokenKind::OctalIntegerLiteral
        }
        else {
            TokenKind::Unknown
        }
    }

    fn hex_integer_literal(&mut self) -> TokenKind {
        if self.eat_hex_digits() {
            TokenKind::HexIntegerLiteral
        } else {
            TokenKind::Unknown
        }
    }

    fn decimal_literal(&mut self) -> TokenKind {
        match self.first() {
            '0'..='9' => {
                self.eat_decimal_integer_literal();
            },
            _ => ()
        };

        match self.first() {
            '.' => {
                self.bump();
                self.eat_decimal_digits();
            }
            _ => ()
        };

        match self.first() {
            'e' | 'E' => {
                self.eat_exponent_part();
            }
            _ => ()
        };

        TokenKind::DecimalLiteral
    }

    fn double_string_literal(&mut self) -> TokenKind {
        loop {
            if self.is_eof() {
                break;
            }
            
            match self.first() {
                '\u{2028}' | '\u{2029}' => {
                    self.bump();
                },
                '\\' => {
                    self.bump();
                    if is_lineterminator(self.first()) {
                        self.eat_line_terminator_sequence();
                    } else {
                        self.eat_escape_sequence();
                    }
                },
                '"' => {
                    self.bump();
                    break;
                },
                _ => {
                    self.bump();
                },
            };
        }
        TokenKind::StringLiteral
    }

    fn single_string_literal(&mut self) -> TokenKind {
        loop {
            if self.is_eof() {
                break;
            }

            match self.first() {
                '\u{2028}' | '\u{2029}' => {
                    self.bump();
                },
                '\\' => {
                    self.bump();
                    if is_lineterminator(self.first()) {
                        self.eat_line_terminator_sequence();
                    } else {
                        self.eat_escape_sequence();
                    }
                },
                '\'' => {
                    self.bump();
                    break;
                },
                _ => {
                    self.bump();
                },
            };
        }
        TokenKind::StringLiteral
    }

    /// Eats symbols while predicate returns true or until the end of file is reached.
    /// Returns amount of eaten symbols.
    fn eat_while<F>(&mut self, mut predicate: F) -> usize
    where
        F: FnMut(char) -> bool,
    {
        let mut eaten: usize = 0;
        while predicate(self.first()) && !self.is_eof() {
            eaten += 1;
            self.bump(); 
        }

        eaten
    }

    fn advance_token(&mut self) -> LexToken {
        let first_char = self.bump().unwrap();
        let token_kind: TokenKind = match first_char {
            // whitespace
            c if is_whitespace(c) => TokenKind::WhiteSpace,
            // LineTerminator
            c if is_lineterminator(c) => TokenKind::LineTerminator,
            '/' => match self.first() {
                '/' => self.single_line_comment(),
                '*' => self.multi_line_comment(),
                _ => TokenKind::Unknown
            },
            // identifier name
            c if is_id_start(c) => self.identifier_name(),
            // punctuator
            '{' => TokenKind::OpenBrace,
            '}' => TokenKind::CloseBrace,
            '(' => TokenKind::OpenParen,
            ')' => TokenKind::CloseParen,
            '[' => TokenKind::OpenBracket,
            ']' => TokenKind::CloseBracket,
            ';' => TokenKind::Semi,
            ',' => TokenKind::Comma,
            '?' => TokenKind::Question,
            ':' => TokenKind::Colon,
            // mutil-char punctuator
            '=' => match self.first() {
                '=' => match self.first() {
                    '=' => {
                        self.bump(); self.bump();
                        TokenKind::Equal3
                    },
                    _ => {
                        self.bump();
                        TokenKind::Equal2
                    }
                },
                _ => TokenKind::Equal
            },
            '0' => match self.first() {
                'b'|'B' => self.binary_integer_literal(),
                'o'|'O' => self.octal_integer_literal(),
                'x'|'X' => self.hex_integer_literal(),
                '.' | '0'..='9' | 'e' | 'E' => self.decimal_literal(),
                _ => TokenKind::Unknown,
            },
            '.' | '1'..='9' => self.decimal_literal(),
            '"' => self.double_string_literal(),
            '\'' => self.single_string_literal(),
            _ => TokenKind::Unknown,
        };
        LexToken::new(token_kind, self.len_consumed())
    }
}