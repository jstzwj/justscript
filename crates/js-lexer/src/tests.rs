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

#[test]
fn lex_hashbang_only_at_start() {
    // A leading `#!` line is a comment; the identifier after it survives.
    let toks = nontrivia("#!/usr/bin/env node\nvar x");
    assert_eq!(toks.len(), 2);
    assert!(matches!(toks[1], TokenKind::Ident(_)));
    // `#!` NOT at the start is not a hashbang (it would be `#`/`!` tokens).
    let toks = nontrivia("x #! y");
    assert!(matches!(toks[0], TokenKind::Ident(_)));
}

#[test]
fn lex_escaped_identifiers() {
    // \uXXXX mid-identifier → cooked text.
    let toks = nontrivia("a\\u0062c");
    match &toks[0] {
        TokenKind::Ident(s) => assert_eq!(s, "abc"),
        other => panic!("expected Ident(abc), got {other:?}"),
    }
    // \u{...} brace form.
    let toks = nontrivia("\\u{61}\\u{62}");
    match &toks[0] {
        TokenKind::Ident(s) => assert_eq!(s, "ab"),
        other => panic!("expected Ident(ab), got {other:?}"),
    }
    // An escaped reserved word yields the Keyword token (accepted in property
    // names, rejected as a binding — the parser already enforces that).
    let toks = nontrivia("x.\\u0063ontinue");
    assert!(matches!(toks[1], TokenKind::Punctuator(js_syntax::Punctuator::Dot)));
    assert!(matches!(&toks[2], TokenKind::Keyword(k) if k.as_str() == "continue"));
}

#[test]
fn lex_numeric_separators() {
    let toks = nontrivia("1_000 0xff_ff");
    match &toks[0] {
        TokenKind::Numeric(s) => assert_eq!(s, "1_000"),
        other => panic!("expected Numeric, got {other:?}"),
    }
    assert!(matches!(&toks[1], TokenKind::Numeric(_)));
}

