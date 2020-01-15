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

// http://unicode.org/cldr/utility/list-unicodeset.jsp?a=[:ID_Start=Yes:]
fn is_unicodeidstart(c:char) -> bool {
    // UnicodeIDStart
    match c{
        ('\u{0041}'..='\u{005A}') | ('\u{0061}'..='\u{007A}') | '\u{00AA}' | '\u{00B5}'
		| '\u{00BA}' | ('\u{00C0}'..='\u{00D6}') | ('\u{00D8}'..='\u{00F6}') | ('\u{00F8}'..='\u{021F}')
		| ('\u{0222}'..='\u{0233}') | ('\u{0250}'..='\u{02AD}') | ('\u{02B0}'..='\u{02B8}') | ('\u{02BB}'..='\u{02C1}')
		| ('\u{02D0}'..='\u{02D1}') | ('\u{02E0}'..='\u{02E4}') | '\u{02EE}' | '\u{037A}'
		| '\u{0386}' | ('\u{0388}'..='\u{038A}') | '\u{038C}' | ('\u{038E}'..='\u{03A1}')
		| ('\u{03A3}'..='\u{03CE}') | ('\u{03D0}'..='\u{03D7}') | ('\u{03DA}'..='\u{03F3}') | ('\u{0400}'..='\u{0481}')
		| ('\u{048C}'..='\u{04C4}') | ('\u{04C7}'..='\u{04C8}') | ('\u{04CB}'..='\u{04CC}') | ('\u{04D0}'..='\u{04F5}')
		| ('\u{04F8}'..='\u{04F9}') | ('\u{0531}'..='\u{0556}') | '\u{0559}' |('\u{0561}'..='\u{0587}')
		| ('\u{05D0}'..='\u{05EA}') | ('\u{05F0}'..='\u{05F2}') | ('\u{0621}'..='\u{063A}') |('\u{0640}'..='\u{064A}')
		| ('\u{0671}'..='\u{06D3}') | '\u{06D5}' | ('\u{06E5}'..='\u{06E6}') |('\u{06FA}'..='\u{06FC}')
		| '\u{0710}' | ('\u{0712}'..='\u{072C}') | ('\u{0780}'..='\u{07A5}') |('\u{0905}'..='\u{0939}')
		| '\u{093D}' | '\u{0950}' | ('\u{0958}'..='\u{0961}') |('\u{0985}'..='\u{098C}')
		| ('\u{098F}'..='\u{0990}') | ('\u{0993}'..='\u{09A8}') | ('\u{09AA}'..='\u{09B0}') | '\u{09B2}'
		| ('\u{09B6}'..='\u{09B9}') | ('\u{09DC}'..='\u{09DD}') | ('\u{09DF}'..='\u{09E1}') |('\u{09F0}'..='\u{09F1}')
		| ('\u{0A05}'..='\u{0A0A}') | ('\u{0A0F}'..='\u{0A10}') | ('\u{0A13}'..='\u{0A28}') |('\u{0A2A}'..='\u{0A30}')
		| ('\u{0A32}'..='\u{0A33}') | ('\u{0A35}'..='\u{0A36}') | ('\u{0A38}'..='\u{0A39}') |('\u{0A59}'..='\u{0A5C}')
		| '\u{0A5E}' | ('\u{0A72}'..='\u{0A74}') | ('\u{0A85}'..='\u{0A8B}') | '\u{0A8D}'
		| ('\u{0A8F}'..='\u{0A91}') | ('\u{0A93}'..='\u{0AA8}') | ('\u{0AAA}'..='\u{0AB0}') |('\u{0AB2}'..='\u{0AB3}')
		| ('\u{0AB5}'..='\u{0AB9}') | '\u{0ABD}' | '\u{0AD0}' | '\u{0AE0}'
		| ('\u{0B05}'..='\u{0B0C}') | ('\u{0B0F}'..='\u{0B10}') | ('\u{0B13}'..='\u{0B28}') |('\u{0B2A}'..='\u{0B30}')
		| ('\u{0B32}'..='\u{0B33}') | ('\u{0B36}'..='\u{0B39}') | '\u{0B3D}' |('\u{0B5C}'..='\u{0B5D}')
		| ('\u{0B5F}'..='\u{0B61}') | ('\u{0B85}'..='\u{0B8A}') | ('\u{0B8E}'..='\u{0B90}') |('\u{0B92}'..='\u{0B95}')
		| ('\u{0B99}'..='\u{0B9A}') | '\u{0B9C}' | ('\u{0B9E}'..='\u{0B9F}') |('\u{0BA3}'..='\u{0BA4}')
		| ('\u{0BA8}'..='\u{0BAA}') | ('\u{0BAE}'..='\u{0BB5}') | ('\u{0BB7}'..='\u{0BB9}') |('\u{0C05}'..='\u{0C0C}')
		| ('\u{0C0E}'..='\u{0C10}') | ('\u{0C12}'..='\u{0C28}') | ('\u{0C2A}'..='\u{0C33}') |('\u{0C35}'..='\u{0C39}')
		| ('\u{0C60}'..='\u{0C61}') | ('\u{0C85}'..='\u{0C8C}') | ('\u{0C8E}'..='\u{0C90}') |('\u{0C92}'..='\u{0CA8}')
		| ('\u{0CAA}'..='\u{0CB3}') | ('\u{0CB5}'..='\u{0CB9}') | '\u{0CDE}' |('\u{0CE0}'..='\u{0CE1}')
		| ('\u{0D05}'..='\u{0D0C}') | ('\u{0D0E}'..='\u{0D10}') | ('\u{0D12}'..='\u{0D28}') |('\u{0D2A}'..='\u{0D39}')
		| ('\u{0D60}'..='\u{0D61}') | ('\u{0D85}'..='\u{0D96}') | ('\u{0D9A}'..='\u{0DB1}') |('\u{0DB3}'..='\u{0DBB}')
		| '\u{0DBD}' | ('\u{0DC0}'..='\u{0DC6}') | ('\u{0E01}'..='\u{0E30}') |('\u{0E32}'..='\u{0E33}')
		| ('\u{0E40}'..='\u{0E46}') | ('\u{0E81}'..='\u{0E82}') | '\u{0E84}' |('\u{0E87}'..='\u{0E88}')
		| '\u{0E8A}' | '\u{0E8D}' | ('\u{0E94}'..='\u{0E97}') |('\u{0E99}'..='\u{0E9F}')
		| ('\u{0EA1}'..='\u{0EA3}') | '\u{0EA5}' | '\u{0EA7}' |('\u{0EAA}'..='\u{0EAB}')
		| ('\u{0EAD}'..='\u{0EB0}') | ('\u{0EB2}'..='\u{0EB3}') | ('\u{0EBD}'..='\u{0EC4}') | '\u{0EC6}'
		| ('\u{0EDC}'..='\u{0EDD}') | '\u{0F00}' | ('\u{0F40}'..='\u{0F6A}') |('\u{0F88}'..='\u{0F8B}')
		| ('\u{1000}'..='\u{1021}') | ('\u{1023}'..='\u{1027}') | ('\u{1029}'..='\u{102A}') |('\u{1050}'..='\u{1055}')
		| ('\u{10A0}'..='\u{10C5}') | ('\u{10D0}'..='\u{10F6}') | ('\u{1100}'..='\u{1159}') |('\u{115F}'..='\u{11A2}')
		| ('\u{11A8}'..='\u{11F9}') | ('\u{1200}'..='\u{1206}') | ('\u{1208}'..='\u{1246}') | '\u{1248}'
		| ('\u{124A}'..='\u{124D}') | ('\u{1250}'..='\u{1256}') | '\u{1258}' |('\u{125A}'..='\u{125D}')
		| ('\u{1260}'..='\u{1286}') | '\u{1288}' | ('\u{128A}'..='\u{128D}') |('\u{1290}'..='\u{12AE}')
		| '\u{12B0}' | ('\u{12B2}'..='\u{12B5}') | ('\u{12B8}'..='\u{12BE}') | '\u{12C0}'
		| ('\u{12C2}'..='\u{12C5}') | ('\u{12C8}'..='\u{12CE}') | ('\u{12D0}'..='\u{12D6}') |('\u{12D8}'..='\u{12EE}')
		| ('\u{12F0}'..='\u{130E}') | '\u{1310}' | ('\u{1312}'..='\u{1315}') |('\u{1318}'..='\u{131E}')
		| ('\u{1320}'..='\u{1346}') | ('\u{1348}'..='\u{135A}') | ('\u{13A0}'..='\u{13B0}') |('\u{13B1}'..='\u{13F4}')
		| ('\u{1401}'..='\u{1676}') | ('\u{1681}'..='\u{169A}') | ('\u{16A0}'..='\u{16EA}') |('\u{1780}'..='\u{17B3}')
		| ('\u{1820}'..='\u{1877}') | ('\u{1880}'..='\u{18A8}') | ('\u{1E00}'..='\u{1E9B}') |('\u{1EA0}'..='\u{1EE0}')
		| ('\u{1EE1}'..='\u{1EF9}') | ('\u{1F00}'..='\u{1F15}') | ('\u{1F18}'..='\u{1F1D}') |('\u{1F20}'..='\u{1F39}')
		| ('\u{1F3A}'..='\u{1F45}') | ('\u{1F48}'..='\u{1F4D}') | ('\u{1F50}'..='\u{1F57}') | '\u{1F59}'
		| '\u{1F5B}' | '\u{1F5D}' | ('\u{1F5F}'..='\u{1F7D}') | ('\u{1F80}'..='\u{1FB4}')
		| ('\u{1FB6}'..='\u{1FBC}') | '\u{1FBE}' | ('\u{1FC2}'..='\u{1FC4}') | ('\u{1FC6}'..='\u{1FCC}')
		| ('\u{1FD0}'..='\u{1FD3}') | ('\u{1FD6}'..='\u{1FDB}') | ('\u{1FE0}'..='\u{1FEC}') | ('\u{1FF2}'..='\u{1FF4}')
		| ('\u{1FF6}'..='\u{1FFC}') | '\u{207F}' | '\u{2102}' | '\u{2107}'
		| ('\u{210A}'..='\u{2113}') | '\u{2115}' | ('\u{2119}'..='\u{211D}') | '\u{2124}'
		| '\u{2126}' | '\u{2128}' | ('\u{212A}'..='\u{212D}') | ('\u{212F}'..='\u{2131}')
		| ('\u{2133}'..='\u{2139}') | ('\u{2160}'..='\u{2183}') | ('\u{3005}'..='\u{3007}') | ('\u{3021}'..='\u{3029}')
		| ('\u{3031}'..='\u{3035}') | ('\u{3038}'..='\u{303A}') | ('\u{3041}'..='\u{3094}') | ('\u{309D}'..='\u{309E}')
		| ('\u{30A1}'..='\u{30FA}') | ('\u{30FC}'..='\u{30FE}') | ('\u{3105}'..='\u{312C}') | ('\u{3131}'..='\u{318E}')
		| ('\u{31A0}'..='\u{31B7}') | '\u{3400}' | '\u{4DB5}' | '\u{4E00}'
		| '\u{9FA5}' | ('\u{A000}'..='\u{A48C}') | '\u{AC00}' | '\u{D7A3}'
		| ('\u{F900}'..='\u{FA2D}') | ('\u{FB00}'..='\u{FB06}') | ('\u{FB13}'..='\u{FB17}') | '\u{FB1D}'
		| ('\u{FB1F}'..='\u{FB28}') | ('\u{FB2A}'..='\u{FB36}') | ('\u{FB38}'..='\u{FB3C}') | '\u{FB3E}'
		| ('\u{FB40}'..='\u{FB41}') | ('\u{FB43}'..='\u{FB44}') | ('\u{FB46}'..='\u{FBB1}') | ('\u{FBD3}'..='\u{FD3D}')
		| ('\u{FD50}'..='\u{FD8F}') | ('\u{FD92}'..='\u{FDC7}') | ('\u{FDF0}'..='\u{FDFB}') | ('\u{FE70}'..='\u{FE72}')
		| '\u{FE74}' | ('\u{FE76}'..='\u{FEFC}') | ('\u{FF21}'..='\u{FF3A}') | ('\u{FF41}'..='\u{FF5A}')
		| ('\u{FF66}'..='\u{FFBE}') | ('\u{FFC2}'..='\u{FFC7}') | ('\u{FFCA}'..='\u{FFCF}') | ('\u{FFD2}'..='\u{FFD7}')
		| ('\u{FFDA}'..='\u{FFDC}') 
        => true,
        _ => false
    }
}

// http://unicode.org/cldr/utility/list-unicodeset.jsp?a=[:ID_Continue=Yes:]
// not finished
fn is_unicodeidcontinue(c:char) -> bool {
    match c{
        '\u{0030}'..='\u{0039}' | '\u{0041}'..='\u{005A}'| '\u{005F}'
        | '\u{0061}'..='\u{007A}' | '\u{00AA}'| '\u{00B5}'| '\u{00B7}'| '\u{00BA}'
        =>true,
        _=>false
    }
}

impl Cursor<'_> {
    // fn lex(&mut self, code: &str) {}
    fn eat_str(iter: &mut Chars, s: &str) -> bool {
        true
    }

    fn eat_unicode_escape_sequence(&mut self) -> bool {
        true
    }

    fn advance_token(&mut self) -> Token {
        let first_char = self.bump().unwrap();
        let token_kind: TokenKind = match first_char {
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