//! Class declaration / expression parsing.
//!
//! `class Name? extends Super? { <members> }`. Members are methods (including
//! the constructor), fields (with optional initializers), `static` variants,
//! accessors (`get`/`set`), computed keys, and private (`#`) names.

use crate::expr;
use crate::token_stream::ParserTokenStream;
use js_diagnostics::Diagnostic;
use js_syntax::ast::expr::{Class, ClassMember, ClassMemberKind, ClassMemberValue, FunctionExpr};
use js_syntax::ast::pat::PropKey;
use js_syntax::keyword::Keyword;
use js_syntax::punctuator::Punctuator;
use js_syntax::source::Span;
use js_syntax::token::TokenKind;

/// Parse a class declaration (the `class` keyword already consumed by caller).
/// `decorators` are class-level decorators parsed by the caller before `class`.
pub fn parse_class_decl(
    tokens: &mut ParserTokenStream,
    start: Span,
    decorators: Vec<js_syntax::ast::expr::Expr>,
) -> Result<js_syntax::ast::ClassDecl, Vec<Diagnostic>> {
    parse_class(tokens, start, decorators)
}

/// Parse a class expression (the `class` keyword already consumed by caller).
/// `decorators` are class-level decorators parsed by the caller before `class`.
pub fn parse_class_expr(
    tokens: &mut ParserTokenStream,
    start: Span,
    decorators: Vec<js_syntax::ast::expr::Expr>,
) -> Result<js_syntax::ast::expr::Expr, Vec<Diagnostic>> {
    let c = parse_class(tokens, start, decorators)?;
    Ok(js_syntax::ast::expr::Expr::Class(Box::new(c)))
}

fn parse_class(
    tokens: &mut ParserTokenStream,
    start: Span,
    decorators: Vec<js_syntax::ast::expr::Expr>,
) -> Result<Class, Vec<Diagnostic>> {
    // A class name is a BindingIdentifier — contextual keywords like `await`
    // (legal in a sloppy script) are accepted via `binding_identifier`, which
    // returns `None` without consuming for an anonymous class (`{`/`extends`).
    let name = expr::binding_identifier(tokens);
    let superclass = if tokens.eat_keyword(Keyword::Extends) {
        let heritage = expr::parse_lhs(tokens)?;
        if matches!(heritage, js_syntax::ast::expr::Expr::Arrow(_)) {
            return Err(vec![Diagnostic::error(
                heritage.span(),
                "an unparenthesized arrow function is not valid class heritage",
            )]);
        }
        Some(Box::new(heritage))
    } else {
        None
    };
    let body = parse_class_body(tokens)?;
    let end = body_end(&body).unwrap_or(start.end);
    Ok(Class {
        span: Span::new(start.start, end),
        name,
        superclass,
        body,
        decorators,
    })
}

/// Parse zero or more leading decorators (`@dec …`), returning them in source
/// order. Used both for class-level decorators (before `class`) and for
/// element-level decorators (before a class member).
pub(crate) fn parse_decorator_list(
    tokens: &mut ParserTokenStream,
) -> Result<Vec<js_syntax::ast::expr::Expr>, Vec<Diagnostic>> {
    let mut out = Vec::new();
    while matches!(tokens.peek_kind(), TokenKind::Punctuator(Punctuator::At)) {
        out.push(parse_decorator(tokens)?);
    }
    Ok(out)
}

/// Parse a single decorator: `@id`, `@id.name`, `@id.#priv`, `@id[expr]`,
/// `@id(args)`, or the parenthesized form `@( Expression )` — a restricted
/// postfix expression stored as an [`Expr`] (parsed, not executed).
fn parse_decorator(
    tokens: &mut ParserTokenStream,
) -> Result<js_syntax::ast::expr::Expr, Vec<Diagnostic>> {
    let start = tokens.span();
    tokens.bump(); // `@`

    use js_syntax::ast::expr::{CallExpr, Expr, MemberExpr, MemberProp};
    let mut node: Expr = if matches!(
        tokens.peek_kind(),
        TokenKind::Punctuator(Punctuator::LParen)
    ) {
        // @( Expression )
        tokens.bump(); // `(`
        let inner = expr::parse_expression(tokens)?;
        let _ = expr::expect_punctuator(tokens, Punctuator::RParen)?;
        Expr::Paren {
            span: Span::new(start.start, tokens.span().start),
            expr: Box::new(inner),
        }
    } else {
        // IdentifierReference: an identifier or a keyword/escaped name used as
        // a reference (`@await`, `@℘`, …).
        let name = decorator_ref_name(tokens)?;
        Expr::Ident { span: start, name }
    };

    // Postfix chain: `.name` / `.#priv` / `[expr]` / `(args)`.
    loop {
        match tokens.peek_kind() {
            TokenKind::Punctuator(Punctuator::Dot) => {
                tokens.bump();
                let (name, is_private) = expr::parse_property_name(tokens)?;
                let prop = if is_private {
                    MemberProp::Private(name)
                } else {
                    MemberProp::Ident(name)
                };
                node = Expr::Member(Box::new(MemberExpr {
                    span: Span::new(start.start, tokens.span().start),
                    object: Box::new(node),
                    property: prop,
                    optional: false,
                }));
            }
            TokenKind::Punctuator(Punctuator::LBracket) => {
                tokens.bump();
                let idx = expr::parse_assignment(tokens)?;
                let close = expr::expect_punctuator(tokens, Punctuator::RBracket)?;
                node = Expr::Member(Box::new(MemberExpr {
                    span: Span::new(start.start, close.end),
                    object: Box::new(node),
                    property: MemberProp::Computed(Box::new(idx)),
                    optional: false,
                }));
            }
            TokenKind::Punctuator(Punctuator::LParen) => {
                let args = expr::parse_call_args(tokens)?;
                node = Expr::Call(Box::new(CallExpr {
                    span: Span::new(start.start, tokens.span().start),
                    callee: Box::new(node),
                    args,
                    optional: false,
                }));
            }
            _ => break,
        }
    }
    Ok(node)
}

/// The identifier reference right after `@` (or after `.`): an `Ident`, a
/// keyword used as a name (`await`/`yield`/…), or a private name (`#x`).
fn decorator_ref_name(tokens: &mut ParserTokenStream) -> Result<String, Vec<Diagnostic>> {
    match tokens.peek_kind().clone() {
        TokenKind::Ident(n) => {
            tokens.bump();
            Ok(n)
        }
        TokenKind::Keyword(kw) => {
            tokens.bump();
            Ok(kw.as_str().to_string())
        }
        TokenKind::PrivateName(n) => {
            tokens.bump();
            Ok(n)
        }
        other => Err(vec![Diagnostic::error(
            tokens.span(),
            format!("expected decorator name, found {:?}", other),
        )]),
    }
}

fn body_end(body: &[ClassMember]) -> Option<js_syntax::source::BytePos> {
    body.last().map(|m| m.span.end)
}

fn parse_class_body(tokens: &mut ParserTokenStream) -> Result<Vec<ClassMember>, Vec<Diagnostic>> {
    let _ = expr::expect_punctuator(tokens, Punctuator::LBrace)?;
    let mut members = Vec::new();
    while !matches!(
        tokens.peek_kind(),
        TokenKind::Punctuator(Punctuator::RBrace) | TokenKind::Eof
    ) {
        // Stray `;` (empty class member) — allowed.
        if tokens.eat_punctuator(Punctuator::Semicolon) {
            continue;
        }
        members.push(parse_class_member(tokens)?);
    }
    let _ = expr::expect_punctuator(tokens, Punctuator::RBrace)?;
    Ok(members)
}

fn parse_class_member(tokens: &mut ParserTokenStream) -> Result<ClassMember, Vec<Diagnostic>> {
    let start = tokens.span();

    // Leading element-level decorators (`@dec method() {}`).
    let decorators = parse_decorator_list(tokens)?;

    // `static` modifier (but `static` may itself be a property name, and
    // `static { ... }` is a static initializer block).
    let mut static_ = false;
    if matches!(tokens.peek_kind(), TokenKind::Keyword(Keyword::Static))
        && !is_member_continuation(&tokens.peek2().kind)
    {
        if matches!(
            tokens.peek2().kind,
            TokenKind::Punctuator(Punctuator::LBrace)
        ) {
            tokens.bump(); // `static`
            let (block, close) = expr::parse_block(tokens)?;
            return Ok(ClassMember {
                span: Span::new(start.start, close.end),
                key: PropKey::Ident(String::new()),
                value: ClassMemberValue::StaticBlock(block),
                static_: true,
                computed: false,
                kind: ClassMemberKind::StaticBlock,
                decorators,
            });
        }
        if tokens.current_token_contains_escape() {
            return Err(vec![Diagnostic::error(
                tokens.span(),
                "the class `static` modifier may not contain an escape sequence",
            )]);
        }
        tokens.bump();
        static_ = true;
    }

    // Stage-3 auto-accessor: `accessor name [= initializer]`. The current AST
    // represents it as a field until auto-accessor runtime semantics exist, but
    // recognizing the modifier here is necessary to preserve its grammar (and
    // keeps ordinary same-line fields invalid).
    let is_auto_accessor = matches!(tokens.peek_kind(), TokenKind::Ident(name) if name == "accessor")
        && !tokens.preceded_by_newline_at(1)
        && is_plain_prop_name_start(&tokens.peek2().kind);
    if is_auto_accessor {
        tokens.bump();
    }

    // `async` method modifier (contextual keyword). It is a modifier only when
    // the upcoming tokens form an async method: `async *name(` or `async name(`.
    // Otherwise `async` is itself the member name (e.g. `async() {}`, `async = 1`).
    let mut is_async = false;
    if matches!(tokens.peek_kind(), TokenKind::Keyword(Keyword::Async))
        && is_async_modifier_ahead(tokens)
    {
        if tokens.current_token_contains_escape() {
            return Err(vec![Diagnostic::error(
                tokens.span(),
                "the method `async` modifier may not contain an escape sequence",
            )]);
        }
        tokens.bump();
        is_async = true;
    }

    // `*` generator marker.
    let is_generator = tokens.eat_punctuator(Punctuator::Mul);

    // get / set accessors (not combinable with async/generator in valid code).
    // `[no LineTerminator here]`: `get`/`set` followed by a newline is a plain
    // field name, not an accessor (e.g. `get\n*a(){}`).
    let mut kind = ClassMemberKind::Method;
    if !is_async && !is_generator {
        if matches!(tokens.peek_kind(), TokenKind::Keyword(Keyword::Get))
            && !tokens.preceded_by_newline_at(1)
            && !is_member_continuation(&tokens.peek2().kind)
        {
            tokens.bump();
            kind = ClassMemberKind::Get;
        } else if matches!(tokens.peek_kind(), TokenKind::Keyword(Keyword::Set))
            && !tokens.preceded_by_newline_at(1)
            && !is_member_continuation(&tokens.peek2().kind)
        {
            tokens.bump();
            kind = ClassMemberKind::Set;
        }
    }

    let (key, computed) = parse_member_key(tokens)?;

    // Method (including constructor).
    if matches!(
        tokens.peek_kind(),
        TokenKind::Punctuator(Punctuator::LParen)
    ) {
        tokens.enter_strict_fn(is_async, is_generator);
        let result = (|| {
            let params = expr::parse_params(tokens)?;
            let (body, close) = expr::parse_block(tokens)?;
            let span = Span::new(start.start, close.end);
            let final_kind = if is_constructor_key(&key)
                && !static_
                && kind == ClassMemberKind::Method
                && !is_async
                && !is_generator
            {
                ClassMemberKind::Constructor
            } else {
                kind
            };
            let name = propkey_name(&key);
            Ok(ClassMember {
                span,
                key,
                value: ClassMemberValue::Method(Box::new(FunctionExpr {
                    span,
                    name,
                    params,
                    body,
                    is_async,
                    is_generator,
                })),
                static_,
                computed,
                kind: final_kind,
                decorators,
            })
        })();
        tokens.pop_ctx();
        return result;
    }

    // Field, with optional initializer.
    let init = if tokens.eat_punctuator(Punctuator::Assign) {
        Some(expr::parse_assignment(tokens)?)
    } else {
        None
    };
    // A class field may omit its semicolon only at a line boundary or directly
    // before the closing brace. Without either, the following token is part of
    // the same ClassElement and makes it invalid (`x y`, `x method(){}`).
    if !tokens.eat_punctuator(Punctuator::Semicolon)
        && !matches!(
            tokens.peek_kind(),
            TokenKind::Punctuator(Punctuator::RBrace)
        )
        && !tokens.preceded_by_newline()
    {
        return Err(vec![Diagnostic::error(
            tokens.span(),
            "expected `;` or a line terminator after class field",
        )]);
    }
    let end = init
        .as_ref()
        .map(|e| e.span().end)
        .unwrap_or_else(|| tokens.span().start);
    Ok(ClassMember {
        span: Span::new(start.start, end),
        key,
        value: ClassMemberValue::Field(init),
        static_,
        computed,
        kind: ClassMemberKind::Field,
        decorators,
    })
}

/// Whether `async` (at the current position) is a method modifier rather than a
/// member name: the next token is `*`, or a plain property-name token that is
/// itself immediately followed by `(`. Shared by class-member and object-literal
/// concise-method parsing.
pub(crate) fn is_async_modifier_ahead(tokens: &ParserTokenStream) -> bool {
    let k2 = &tokens.peek2().kind;
    if matches!(k2, TokenKind::Punctuator(Punctuator::Mul)) {
        return true;
    }
    if is_plain_prop_name_start(k2) {
        return matches!(
            tokens.peek3().kind,
            TokenKind::Punctuator(Punctuator::LParen)
        );
    }
    // Computed-key async method: `async [expr](`. Scan past the bracket-balanced
    // `[...]` and check that a `(` follows.
    if matches!(k2, TokenKind::Punctuator(Punctuator::LBracket)) {
        let mut depth = 0isize;
        let mut i = 1; // start at the `[` (== peek2, offset 1 from current)
        loop {
            let k = &tokens.peek_at(i).kind;
            match k {
                TokenKind::Punctuator(Punctuator::LBracket) => depth += 1,
                TokenKind::Punctuator(Punctuator::RBracket) => {
                    depth -= 1;
                    if depth == 0 {
                        return matches!(
                            tokens.peek_at(i + 1).kind,
                            TokenKind::Punctuator(Punctuator::LParen)
                        );
                    }
                }
                TokenKind::Eof => return false,
                _ => {}
            }
            i += 1;
        }
    }
    false
}

/// A property-name token usable for the simple `async name(` lookahead (excludes
/// computed `[...]` async names, which are rare).
pub(crate) fn is_plain_prop_name_start(kind: &TokenKind) -> bool {
    matches!(
        kind,
        TokenKind::Ident(_)
            | TokenKind::Keyword(_)
            | TokenKind::String(_)
            | TokenKind::Numeric(_)
            | TokenKind::PrivateName(_)
    )
}

/// A class member key: `[computed]`, identifier, private `#name`, string,
/// number, or a keyword used as a name.
fn parse_member_key(tokens: &mut ParserTokenStream) -> Result<(PropKey, bool), Vec<Diagnostic>> {
    if tokens.eat_punctuator(Punctuator::LBracket) {
        let e = expr::parse_assignment(tokens)?;
        let _ = expr::expect_punctuator(tokens, Punctuator::RBracket)?;
        return Ok((PropKey::Computed(Box::new(e)), true));
    }
    match tokens.peek_kind().clone() {
        TokenKind::Ident(n) => {
            tokens.bump();
            Ok((PropKey::Ident(n), false))
        }
        TokenKind::PrivateName(n) => {
            tokens.bump();
            Ok((PropKey::Private(n), false))
        }
        TokenKind::Keyword(kw) => {
            tokens.bump();
            Ok((PropKey::Ident(kw.as_str().to_string()), false))
        }
        TokenKind::String(s) => {
            tokens.bump();
            Ok((PropKey::String(s), false))
        }
        TokenKind::Numeric(raw) => {
            tokens.bump();
            Ok((
                PropKey::Number(js_lexer::parse_number(&raw).unwrap_or(f64::NAN)),
                false,
            ))
        }
        TokenKind::Bigint(raw) => {
            tokens.bump();
            Ok((PropKey::String(expr::bigint_property_name(&raw)), false))
        }
        other => Err(vec![Diagnostic::error(
            tokens.span(),
            format!("expected class member name, found {:?}", other),
        )]),
    }
}

fn propkey_name(key: &PropKey) -> Option<String> {
    match key {
        PropKey::Ident(n) | PropKey::String(n) | PropKey::Private(n) => Some(n.clone()),
        PropKey::Number(n) => Some(n.to_string()),
        PropKey::Computed(_) => None,
    }
}

fn is_constructor_key(key: &PropKey) -> bool {
    matches!(key, PropKey::Ident(n) if n == "constructor")
}

/// Whether the token after `static`/`get`/`set` could *not* begin a member key,
/// meaning the keyword is itself the member name.
fn is_member_continuation(kind: &TokenKind) -> bool {
    matches!(
        kind,
        TokenKind::Punctuator(Punctuator::LParen)
            | TokenKind::Punctuator(Punctuator::Semicolon)
            | TokenKind::Punctuator(Punctuator::Assign)
            | TokenKind::Punctuator(Punctuator::RBrace)
            | TokenKind::Punctuator(Punctuator::Comma)
    )
}
