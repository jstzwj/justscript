//! A character-level cursor over source text.
//!
//! Inspired by `rustc_lexer::Cursor`. It operates on `char`s but tracks the
//! byte offset so spans can be produced cheaply. The cursor never advances
//! past EOF and always yields `'\0'` when asked for a character beyond the end.

use js_syntax::BytePos;

/// The EOF sentinel returned at end of input.
pub const EOF_CHAR: char = '\0';

pub struct Cursor<'a> {
    /// A `Chars` iterator so we can cheaply peek the next char.
    chars: std::str::Chars<'a>,
    /// Source text, retained for offset bookkeeping.
    start: usize,
    /// Byte offset of the *start* of the next char to be consumed.
    pos: usize,
    /// The next char (`peek`), or `EOF_CHAR` at end of input.
    ahead: [char; 2],
}

impl<'a> Cursor<'a> {
    pub fn new(src: &'a str) -> Cursor<'a> {
        let mut chars = src.chars();
        let c0 = chars.next().unwrap_or(EOF_CHAR);
        let c1 = chars.next().unwrap_or(EOF_CHAR);
        Cursor {
            chars,
            start: src.as_ptr() as usize,
            pos: 0,
            ahead: [c0, c1],
        }
    }

    /// A cursor over `src` positioned at `byte_offset` (the next char to consume
    /// is the one starting at that byte). Used by the parser to re-lex an
    /// ambiguous `/` token range under a different regex/division goal than the
    /// one the lexer originally assumed. The cursor reports *absolute* byte
    /// offsets (relative to `src`), so re-lexed token spans line up with the
    /// original token's span.
    pub fn new_at(src: &'a str, byte_offset: usize) -> Cursor<'a> {
        let suffix: &'a str = &src[byte_offset..];
        let mut chars = suffix.chars();
        let c0 = chars.next().unwrap_or(EOF_CHAR);
        let c1 = chars.next().unwrap_or(EOF_CHAR);
        Cursor {
            chars,
            start: src.as_ptr() as usize,
            pos: byte_offset,
            ahead: [c0, c1],
        }
    }

    /// The current (not yet consumed) char, or `EOF_CHAR`.
    #[inline]
    pub fn first(&self) -> char {
        self.ahead[0]
    }

    /// The char after [`first`](Self::first), or `EOF_CHAR`.
    #[inline]
    pub fn second(&self) -> char {
        self.ahead[1]
    }

    /// The next four upcoming chars, padded with `EOF_CHAR` past end of input.
    /// Used for maximal-munch punctuator lookahead (the longest ECMAScript
    /// punctuator is `>>>=`, 4 chars) without consuming. Returns chars (not
    /// bytes) so slicing is always char-boundary safe.
    pub fn peek_ahead4(&self) -> [char; 4] {
        let mut out = [EOF_CHAR; 4];
        out[0] = self.ahead[0];
        out[1] = self.ahead[1];
        let mut it = self.chars.clone();
        out[2] = it.next().unwrap_or(EOF_CHAR);
        out[3] = it.next().unwrap_or(EOF_CHAR);
        out
    }

    /// Whether the cursor is at end of input.
    #[inline]
    pub fn is_eof(&self) -> bool {
        self.first() == EOF_CHAR
    }

    /// Byte offset of [`first`](Self::first) — i.e. of the next char to consume.
    #[inline]
    pub fn byte_offset(&self) -> BytePos {
        BytePos(self.pos as u32)
    }

    /// Consume and return the current char. Advances by one char.
    pub fn bump(&mut self) -> char {
        let c = self.ahead[0];
        if c != EOF_CHAR {
            self.pos += c.len_utf8();
            // Slide the lookahead window forward.
            self.ahead[0] = self.ahead[1];
            self.ahead[1] = self.chars.next().unwrap_or(EOF_CHAR);
        }
        c
    }

    /// Consume `c` only if [`first`](Self::first) equals it.
    #[inline]
    pub fn eat(&mut self, c: char) -> bool {
        if self.first() == c {
            self.bump();
            true
        } else {
            false
        }
    }

    /// Consume the next char only if it satisfies `pred`.
    pub fn eat_while<F: Fn(char) -> bool>(&mut self, pred: F) {
        while pred(self.first()) && self.first() != EOF_CHAR {
            self.bump();
        }
    }

    /// Reset the start position used by [`len_consumed`](Self::len_consumed).
    #[inline]
    pub fn reset_len(&mut self) {
        self.start = self.pos;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bumps_chars_and_tracks_bytes() {
        let mut c = Cursor::new("abc");
        assert_eq!(c.first(), 'a');
        assert_eq!(c.byte_offset(), BytePos(0));
        assert_eq!(c.bump(), 'a');
        assert_eq!(c.byte_offset(), BytePos(1));
        assert_eq!(c.bump(), 'b');
        assert_eq!(c.bump(), 'c');
        assert!(c.is_eof());
        assert_eq!(c.bump(), EOF_CHAR);
    }

    #[test]
    fn tracks_multibyte_offsets() {
        let mut c = Cursor::new("é"); // 2 bytes
        assert_eq!(c.bump(), 'é');
        assert_eq!(c.byte_offset(), BytePos(2));
    }
}
