use crate::{tokenize, Cursor};
use js_syntax::token::TokenKind;

fn nontrivia(src: &str) -> Vec<TokenKind> {
    tokenize(src)
        .filter(|t| !t.kind.is_trivia() && !matches!(t.kind, TokenKind::Eof))
        .map(|t| t.kind)
        .collect()
}

#[test]
fn cursor_smoke() {
    let mut c = Cursor::new("ab");
    assert_eq!(c.bump(), 'a');
    assert_eq!(c.bump(), 'b');
    assert!(c.is_eof());
}

#[test]
fn lex_ident_and_keyword() {
    let toks = nontrivia("foo return");
    assert!(matches!(toks[0], TokenKind::Ident(_)));
    assert!(matches!(toks[1], TokenKind::Keyword(_)));
}

#[test]
fn lex_numbers() {
    let toks = nontrivia("42 0x1F 1.5e3 7n");
    assert_eq!(toks.len(), 4);
    assert!(matches!(toks[3], TokenKind::Bigint(_)));
}

#[test]
fn lex_string_unescapes() {
    let toks = nontrivia("'a\\nb'");
    match &toks[0] {
        TokenKind::String(s) => assert_eq!(s, "a\nb"),
        other => panic!("expected string, got {other:?}"),
    }
}

#[test]
fn lex_comments() {
    let toks = nontrivia("// hi\n/* block */ x");
    assert!(matches!(toks[0], TokenKind::Ident(_)));
}
