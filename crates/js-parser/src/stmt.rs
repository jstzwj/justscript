//! Statement parsing.
//!
//! Statement-level grammar is driven by [`parse_statement`] / dispatch on the
//! current keyword. Expression statements fall through to [`crate::expr`].

use crate::expr;
use crate::token_stream::ParserTokenStream;
use js_diagnostics::Diagnostic;
use js_syntax::ast::expr::{Expr, ImportPhase};
use js_syntax::ast::lit::Lit;
use js_syntax::ast::pat::Pat;
use js_syntax::ast::stmt::{
    CatchClause, Decl, ExportItem, ExportSpec, ForInit, ForTarget, ImportAttribute, ImportItem,
    ImportSpec, ModuleExportName, ModuleRequest, Stmt, SwitchCase, TryBlock, VarDeclarator,
    VarKind,
};
use js_syntax::ast::{FunctionDecl, ProgramItem};
use js_syntax::keyword::Keyword;
use js_syntax::punctuator::Punctuator;
use js_syntax::source::Span;
use js_syntax::token::TokenKind;

/// Parse one statement.
pub fn parse_statement(tokens: &mut ParserTokenStream) -> Result<Stmt, Vec<Diagnostic>> {
    parse_statement_inner(tokens, false)
}

/// Parse a StatementListItem, where declarations are grammar alternatives in
/// addition to ordinary statements. This distinction matters for `let` plus a
/// line terminator: a block item continues as a declaration, while an unbraced
/// statement body may parse `let` as an expression and insert a semicolon.
pub(crate) fn parse_statement_list_item(
    tokens: &mut ParserTokenStream,
) -> Result<Stmt, Vec<Diagnostic>> {
    parse_statement_inner(tokens, true)
}

fn parse_statement_inner(
    tokens: &mut ParserTokenStream,
    in_statement_list: bool,
) -> Result<Stmt, Vec<Diagnostic>> {
    let span = tokens.span();

    if matches!(tokens.peek_kind(), TokenKind::Keyword(Keyword::Let))
        && tokens.preceded_by_newline_at(1)
        && matches!(
            tokens.peek2().kind,
            TokenKind::Punctuator(Punctuator::LBracket)
        )
    {
        return Err(vec![Diagnostic::error(
            span,
            "an expression statement may not begin with the token sequence `let [`",
        )]);
    }

    // Block statement.
    if matches!(
        tokens.peek_kind(),
        TokenKind::Punctuator(Punctuator::LBrace)
    ) {
        let (body, close) = expr::parse_block(tokens)?;
        let span = Span::new(span.start, close.end);
        return Ok(Stmt::Block { span, body });
    }

    // Empty statement `;`.
    if tokens.eat_punctuator(Punctuator::Semicolon) {
        return Ok(Stmt::Empty(span));
    }

    // Labeled statement: `ident : stmt`. Detected at the top level (NOT inside
    // the keyword block below) because a label is an Identifier, not a keyword.
    // The `peek2 == ':'` guard avoids misreading e.g. object-literal keys.
    if kw_ppeks_colon(tokens) {
        // The label may be an `Ident` or a contextual keyword; extract its
        // spelling either way.
        let label = match tokens.bump().kind {
            TokenKind::Ident(n) => n,
            TokenKind::Keyword(k) => k.as_str().to_string(),
            _ => unreachable!(),
        };
        tokens.bump(); // `:`
        let body = Box::new(parse_statement(tokens)?);
        let end = body.span();
        return Ok(Stmt::Labeled {
            span: Span::new(span.start, end.end),
            label,
            body,
        });
    }

    // `using` / `await using` lexical declarations (explicit resource
    // management). `using` is a contextual keyword — see `using_decl_kind`.
    if let Some(kind) = using_decl_kind(tokens) {
        let decl = parse_using_decl(tokens, span, kind)?;
        return Ok(Stmt::Decl(Box::new(decl)));
    }

    // Decorated class declaration: `@dec class C {}` (stage-3 decorators).
    if matches!(tokens.peek_kind(), TokenKind::Punctuator(Punctuator::At)) {
        let decorators = crate::class::parse_decorator_list(tokens)?;
        if !matches!(tokens.peek_kind(), TokenKind::Keyword(Keyword::Class)) {
            return Err(vec![Diagnostic::error(
                tokens.span(),
                "a decorator may only precede a class declaration",
            )]);
        }
        tokens.bump(); // `class`
        return Ok(Stmt::Decl(Box::new(Decl::Class(Box::new(
            crate::class::parse_class_decl(tokens, span, decorators)?,
        )))));
    }

    // Keyword-led statements.
    if let TokenKind::Keyword(kw) = tokens.peek_kind().clone() {
        // `async function` declaration: hoisted like a function declaration.
        if matches!(kw, Keyword::Async) && async_function_ahead(tokens) {
            tokens.bump(); // `async`
            tokens.bump(); // `function`
            let decl = parse_function_decl(tokens, span, true, false)?;
            return Ok(Stmt::Decl(Box::new(decl)));
        }
        // Only consume the keyword for actual statement-leading keywords.
        // `let` is a statement keyword only when it introduces a lexical
        // declaration; otherwise (`let;`, `let = 1;`, `let.x;`) it is a plain
        // identifier reference and an expression statement.
        if !is_statement_keyword(kw)
            || (kw == Keyword::Let && !let_is_declaration_ahead(tokens, in_statement_list))
        {
            return parse_expr_statement(tokens, span);
        }
        tokens.bump();
        let stmt = match kw {
            Keyword::Var => Stmt::Decl(Box::new(parse_var(tokens, span, VarKind::Var)?)),
            Keyword::Let => Stmt::Decl(Box::new(parse_var(tokens, span, VarKind::Let)?)),
            Keyword::Const => Stmt::Decl(Box::new(parse_var(tokens, span, VarKind::Const)?)),
            Keyword::Return => parse_return(tokens, span)?,
            Keyword::Function => {
                Stmt::Decl(Box::new(parse_function_decl(tokens, span, false, false)?))
            }
            Keyword::Class => Stmt::Decl(Box::new(Decl::Class(Box::new(
                crate::class::parse_class_decl(tokens, span, Vec::new())?,
            )))),
            Keyword::If => parse_if(tokens, span)?,
            Keyword::While => parse_while(tokens, span)?,
            Keyword::Do => parse_do_while(tokens, span)?,
            Keyword::For => parse_for(tokens, span)?,
            Keyword::Switch => parse_switch(tokens, span)?,
            Keyword::Try => parse_try(tokens, span)?,
            Keyword::Throw => parse_throw(tokens, span)?,
            Keyword::Break => {
                let label = optional_label(tokens);
                expr::consume_asi(tokens)?;
                Stmt::Break { span, label }
            }
            Keyword::Continue => {
                let label = optional_label(tokens);
                expr::consume_asi(tokens)?;
                Stmt::Continue { span, label }
            }
            Keyword::Debugger => {
                expr::consume_asi(tokens)?;
                Stmt::Debugger(span)
            }
            Keyword::With => parse_with(tokens, span)?,
            _ => {
                // Not a statement keyword — treat the keyword as the start of
                // an expression statement (e.g. `this`/`true`/...).
                return parse_expr_statement_with_prefix_keyword(tokens, kw, span);
            }
        };
        return Ok(stmt);
    }

    // Expression statement.
    parse_expr_statement(tokens, span)
}

/// Whether `kw` introduces a statement (and thus should be consumed by the
/// statement dispatcher). Anything else (`new`, `typeof`, `delete`, `this`,
/// `super`, literals, `await`, `yield`, ...) starts an expression statement.
fn is_statement_keyword(kw: Keyword) -> bool {
    matches!(
        kw,
        Keyword::Var
            | Keyword::Let
            | Keyword::Const
            | Keyword::Return
            | Keyword::Function
            | Keyword::Class
            | Keyword::If
            | Keyword::While
            | Keyword::Do
            | Keyword::For
            | Keyword::Switch
            | Keyword::Try
            | Keyword::Throw
            | Keyword::Break
            | Keyword::Continue
            | Keyword::Debugger
            | Keyword::With
    )
}

/// `async function` and `async function*` start with two literal grammar
/// terminals separated by no LineTerminator. Cooked keyword values are not
/// enough here: `\u0061sync` remains an IdentifierName, not the `async`
/// terminal.
fn async_function_ahead(tokens: &ParserTokenStream) -> bool {
    tokens.is_unescaped_keyword_at(0, Keyword::Async)
        && tokens.is_unescaped_keyword_at(1, Keyword::Function)
        && !tokens.preceded_by_newline_at(1)
}

/// True when the current token is a label identifier immediately followed by
/// `:`. A label may be an `Ident` or a contextual keyword usable as a binding
/// (`await`/`yield`/`let`/`static`/`async`/`of`/…), per `LabelIdentifier`.
fn kw_ppeks_colon(tokens: &ParserTokenStream) -> bool {
    // `[no LineTerminator here]` between the label and `:`.
    if tokens.preceded_by_newline_at(1) {
        return false;
    }
    if !matches!(tokens.peek_kind(), TokenKind::Ident(_))
        && expr::peek_binding_name(tokens).is_none()
    {
        return false;
    }
    matches!(
        tokens.peek2().kind,
        TokenKind::Punctuator(Punctuator::Colon)
    )
}

/// Whether the current position starts a `using` / `await using` lexical
/// declaration (explicit resource management). `using` is a contextual
/// keyword: it introduces a declaration only when followed by a binding
/// pattern (`x`, `[`, `{`), not when used as a plain identifier
/// (`using = 1`, `using.x`, `using(...)`).
fn using_decl_kind(tokens: &ParserTokenStream) -> Option<VarKind> {
    fn binding_identifier_start(tokens: &ParserTokenStream, offset: usize) -> bool {
        match &tokens.peek_at(offset).kind {
            TokenKind::Ident(_) => true,
            TokenKind::Keyword(keyword) => {
                expr::keyword_binding_name(*keyword, tokens.current_ctx()).is_some()
            }
            _ => false,
        }
    }
    // `await using x = …` (valid only in async; legality is checked later).
    // `[no LineTerminator here]` between `await`/`using` and the binding.
    if matches!(tokens.peek_kind(), TokenKind::Keyword(Keyword::Await))
        && !tokens.preceded_by_newline_at(1)
        && matches!(tokens.peek2().kind, TokenKind::Ident(ref n) if n == "using")
        && !tokens.preceded_by_newline_at(2)
        && binding_identifier_start(tokens, 2)
    {
        return Some(VarKind::AwaitUsing);
    }
    // `using x = …` / `using { x } = …` / `using [x] = …`. A line terminator
    // between `using` and the binding makes `using` a plain identifier.
    if let TokenKind::Ident(ref n) = tokens.peek_kind() {
        if n == "using" && !tokens.preceded_by_newline_at(1) && binding_identifier_start(tokens, 1)
        {
            return Some(VarKind::Using);
        }
    }
    None
}

/// Whether `let` (the current token) introduces a `let` lexical declaration
/// rather than being a plain identifier reference (sloppy mode). Per the spec
/// `let` is a declaration when followed by a binding: `{`/`[` (destructuring)
/// or an identifier-name binding. Otherwise (`let in`, `let =`, `let .`, …)
/// it is an identifier.
fn let_is_declaration_ahead(tokens: &ParserTokenStream, allow_binding_after_newline: bool) -> bool {
    if !tokens.is_unescaped_keyword_at(0, Keyword::Let) {
        return false;
    }
    let newline = tokens.preceded_by_newline_at(1);
    match &tokens.peek2().kind {
        // `let\n{}` may remain an expression statement followed by a block.
        // The `let [` lookahead restriction applies even across a line break.
        TokenKind::Punctuator(Punctuator::LBrace) => !newline,
        TokenKind::Punctuator(Punctuator::LBracket) => true,
        TokenKind::Ident(_) => allow_binding_after_newline || !newline,
        // `let <contextual-keyword binding>` — e.g. `let async = …`,
        // `let await = …`. `await` and `yield` still have the syntactic shape
        // of BindingIdentifier when their grammar parameter is set; contextual
        // early errors reject them later. They must therefore participate in
        // declaration lookahead even when they are illegal in this context.
        TokenKind::Keyword(Keyword::Await | Keyword::Yield) => {
            allow_binding_after_newline || !newline
        }
        TokenKind::Keyword(k) => {
            (allow_binding_after_newline || !newline)
                && expr::keyword_binding_name(*k, tokens.current_ctx()).is_some()
        }
        _ => false,
    }
}

/// Parse a `using`/`await using` declaration, consuming the leading keywords.
fn parse_using_decl(
    tokens: &mut ParserTokenStream,
    start: Span,
    kind: VarKind,
) -> Result<Decl, Vec<Diagnostic>> {
    if matches!(kind, VarKind::AwaitUsing) {
        tokens.bump(); // `await`
    }
    tokens.bump(); // `using`
    parse_var(tokens, start, kind)
}

/// Parse a top-level [`ProgramItem`].
pub fn parse_program_item(tokens: &mut ParserTokenStream) -> Result<ProgramItem, Vec<Diagnostic>> {
    let span = tokens.span();
    // Decorated class declaration at the top level: `@dec class C {}`.
    if matches!(tokens.peek_kind(), TokenKind::Punctuator(Punctuator::At)) {
        let decorators = crate::class::parse_decorator_list(tokens)?;
        if !matches!(tokens.peek_kind(), TokenKind::Keyword(Keyword::Class)) {
            return Err(vec![Diagnostic::error(
                tokens.span(),
                "a decorator may only precede a class declaration",
            )]);
        }
        tokens.bump(); // `class`
        let c = crate::class::parse_class_decl(tokens, span, decorators)?;
        return Ok(ProgramItem::Decl(Decl::Class(Box::new(c))));
    }
    // Hoisted declarations become ProgramItem::Decl; everything else is a Stmt.
    if let TokenKind::Keyword(kw) = tokens.peek_kind().clone() {
        match kw {
            Keyword::Function | Keyword::Class => {
                let span = tokens.span();
                tokens.bump();
                if kw == Keyword::Function {
                    let decl = parse_function_decl(tokens, span, false, false)?;
                    return Ok(ProgramItem::Decl(decl));
                }
                let c = crate::class::parse_class_decl(tokens, span, Vec::new())?;
                return Ok(ProgramItem::Decl(Decl::Class(Box::new(c))));
            }
            Keyword::Async if async_function_ahead(tokens) => {
                let span = tokens.span();
                tokens.bump(); // async
                tokens.bump(); // function
                let decl = parse_function_decl(tokens, span, true, false)?;
                return Ok(ProgramItem::Decl(decl));
            }
            Keyword::Var | Keyword::Let | Keyword::Const
                if !(kw == Keyword::Let && !let_is_declaration_ahead(tokens, true)) =>
            {
                let kind = match kw {
                    Keyword::Var => VarKind::Var,
                    Keyword::Let => VarKind::Let,
                    _ => VarKind::Const,
                };
                let span = tokens.span();
                tokens.bump();
                let decl = parse_var(tokens, span, kind)?;
                return Ok(ProgramItem::Decl(decl));
            }
            Keyword::Import => {
                // Could be a dynamic import expression — `import(...)`,
                // `import.meta`, `import.source(...)`, `import.defer(...)` — or
                // a static import declaration. Only the declaration is a
                // ProgramItem; the expressions fall through to statement
                // parsing. Distinguish by the token following `import`.
                let is_expr = match tokens.peek2().kind {
                    TokenKind::Punctuator(Punctuator::LParen) => true, // import(...)
                    TokenKind::Punctuator(Punctuator::Dot) => true, // import.meta / .source / .defer
                    _ => false,
                };
                if !is_expr {
                    let decl = parse_import(tokens, span)?;
                    return Ok(ProgramItem::Decl(decl));
                }
            }
            Keyword::Export => {
                let decl = parse_export(tokens, span)?;
                return Ok(ProgramItem::Decl(decl));
            }
            _ => {}
        }
    }
    let stmt = parse_statement(tokens)?;
    Ok(ProgramItem::Stmt(stmt))
}

/// An IdentifierName: an identifier or any keyword spelled out (`default`,
/// `await`, …). Used for the *imported* / *exported* names in import/export
/// specifiers, which permit reserved words.
fn ident_name(tokens: &mut ParserTokenStream) -> Result<String, Vec<Diagnostic>> {
    match tokens.peek_kind().clone() {
        TokenKind::Ident(n) => {
            tokens.bump();
            Ok(n)
        }
        TokenKind::Keyword(k) => {
            tokens.bump();
            Ok(k.as_str().to_string())
        }
        other => Err(vec![Diagnostic::error(
            tokens.span(),
            format!("expected an identifier name, found {:?}", other),
        )]),
    }
}

fn module_export_name(tokens: &mut ParserTokenStream) -> Result<ModuleExportName, Vec<Diagnostic>> {
    let span = tokens.span();
    match tokens.peek_kind().clone() {
        TokenKind::Ident(name) => {
            tokens.bump();
            Ok(ModuleExportName::Identifier(name))
        }
        TokenKind::Keyword(keyword) => {
            tokens.bump();
            Ok(ModuleExportName::Identifier(keyword.as_str().to_string()))
        }
        TokenKind::String(value) => {
            let well_formed = tokens
                .token_span_snippet(span)
                .is_some_and(string_literal_value_is_well_formed);
            if !well_formed {
                return Err(vec![Diagnostic::error(
                    span,
                    "a string ModuleExportName must be well-formed Unicode",
                )]);
            }
            tokens.bump();
            Ok(ModuleExportName::String(value))
        }
        other => Err(vec![Diagnostic::error(
            span,
            format!("expected a module export name, found {:?}", other),
        )]),
    }
}

fn eat_unescaped_keyword(tokens: &mut ParserTokenStream, keyword: Keyword) -> bool {
    if tokens.is_unescaped_keyword_at(0, keyword) {
        tokens.bump();
        true
    } else {
        false
    }
}

/// `from "module" WithClause?` — consume a complete module request.
fn parse_module_request(tokens: &mut ParserTokenStream) -> Result<ModuleRequest, Vec<Diagnostic>> {
    parse_module_request_at_phase(tokens, ImportPhase::Eval)
}

fn parse_module_request_at_phase(
    tokens: &mut ParserTokenStream,
    phase: ImportPhase,
) -> Result<ModuleRequest, Vec<Diagnostic>> {
    if !eat_unescaped_keyword(tokens, Keyword::From) {
        return Err(vec![Diagnostic::error(
            tokens.span(),
            "expected `from` in import/export",
        )]);
    }
    parse_module_request_tail(tokens, phase)
}

fn parse_module_request_tail(
    tokens: &mut ParserTokenStream,
    phase: ImportPhase,
) -> Result<ModuleRequest, Vec<Diagnostic>> {
    let specifier = match tokens.peek_kind().clone() {
        TokenKind::String(src) => {
            tokens.bump();
            src
        }
        other => Err(vec![Diagnostic::error(
            tokens.span(),
            format!("expected a module specifier string, found {:?}", other),
        )])?,
    };
    let attributes = parse_with_clause(tokens)?;
    Ok(ModuleRequest {
        specifier,
        phase,
        attributes,
    })
}

fn parse_with_clause(
    tokens: &mut ParserTokenStream,
) -> Result<Vec<ImportAttribute>, Vec<Diagnostic>> {
    if !eat_unescaped_keyword(tokens, Keyword::With) {
        return Ok(Vec::new());
    }
    expr::expect_punctuator(tokens, Punctuator::LBrace)?;
    let mut attributes = Vec::new();
    while !matches!(
        tokens.peek_kind(),
        TokenKind::Punctuator(Punctuator::RBrace)
    ) {
        let start = tokens.span();
        let key = match tokens.peek_kind().clone() {
            TokenKind::String(key) => {
                tokens.bump();
                key
            }
            TokenKind::Ident(_) | TokenKind::Keyword(_) => ident_name(tokens)?,
            other => {
                return Err(vec![Diagnostic::error(
                    tokens.span(),
                    format!("expected an import attribute key, found {:?}", other),
                )]);
            }
        };
        expr::expect_punctuator(tokens, Punctuator::Colon)?;
        let (value, end) = match tokens.peek_kind().clone() {
            TokenKind::String(value) => {
                let span = tokens.span();
                tokens.bump();
                (value, span.end)
            }
            other => {
                return Err(vec![Diagnostic::error(
                    tokens.span(),
                    format!(
                        "expected a string import attribute value, found {:?}",
                        other
                    ),
                )]);
            }
        };
        attributes.push(ImportAttribute {
            span: Span::new(start.start, end),
            key,
            value,
        });
        if !tokens.eat_punctuator(Punctuator::Comma) {
            break;
        }
        if matches!(
            tokens.peek_kind(),
            TokenKind::Punctuator(Punctuator::RBrace)
        ) {
            break;
        }
    }
    expr::expect_punctuator(tokens, Punctuator::RBrace)?;
    Ok(attributes)
}

/// Module export string names must denote well-formed Unicode strings. The
/// lexer stores cooked values as Rust UTF-8 and therefore substitutes lone
/// UTF-16 surrogates; inspect the raw literal here so that distinction is not
/// lost before applying the module-specific Early Error.
fn string_literal_value_is_well_formed(raw: &str) -> bool {
    let Some(body) = raw.get(1..raw.len().saturating_sub(1)) else {
        return false;
    };
    let mut chars = body.chars().peekable();
    let mut units = Vec::new();
    while let Some(ch) = chars.next() {
        if ch != '\\' {
            let mut encoded = [0; 2];
            units.extend(ch.encode_utf16(&mut encoded).iter().copied());
            continue;
        }

        let Some(escape) = chars.next() else {
            return false;
        };
        if escape == 'u' {
            let value = if chars.next_if_eq(&'{').is_some() {
                let mut hex = String::new();
                loop {
                    match chars.next() {
                        Some('}') if !hex.is_empty() => break,
                        Some(digit) if digit.is_ascii_hexdigit() => hex.push(digit),
                        _ => return false,
                    }
                }
                u32::from_str_radix(&hex, 16).ok()
            } else {
                let hex: String = chars.by_ref().take(4).collect();
                (hex.len() == 4)
                    .then(|| u32::from_str_radix(&hex, 16).ok())
                    .flatten()
            };
            let Some(value) = value.filter(|value| *value <= 0x10ffff) else {
                return false;
            };
            if value <= 0xffff {
                units.push(value as u16);
            } else {
                let value = value - 0x10000;
                units.push(0xd800 | ((value >> 10) as u16));
                units.push(0xdc00 | ((value & 0x3ff) as u16));
            }
        } else if matches!(escape, '\n' | '\u{2028}' | '\u{2029}') {
            // LineContinuation contributes no code units.
        } else if escape == '\r' {
            let _ = chars.next_if_eq(&'\n');
        } else {
            // Every non-Unicode escape contributes a non-surrogate code unit.
            units.push(0);
        }
    }

    let mut index = 0;
    while index < units.len() {
        match units[index] {
            0xd800..=0xdbff => {
                if !matches!(units.get(index + 1), Some(0xdc00..=0xdfff)) {
                    return false;
                }
                index += 2;
            }
            0xdc00..=0xdfff => return false,
            _ => index += 1,
        }
    }
    true
}

/// `{ a, b as c, ... }` import bindings.
fn parse_import_items(tokens: &mut ParserTokenStream) -> Result<Vec<ImportItem>, Vec<Diagnostic>> {
    let _ = expr::expect_punctuator(tokens, Punctuator::LBrace)?;
    let mut items = Vec::new();
    while !matches!(
        tokens.peek_kind(),
        TokenKind::Punctuator(Punctuator::RBrace)
    ) {
        let shorthand_local = expr::peek_binding_name(tokens);
        let imported = module_export_name(tokens)?;
        let local = if eat_unescaped_keyword(tokens, Keyword::As) {
            expr::binding_identifier(tokens).ok_or_else(|| {
                vec![Diagnostic::error(
                    tokens.span(),
                    "expected a binding name after `as`",
                )]
            })?
        } else {
            shorthand_local.ok_or_else(|| {
                vec![Diagnostic::error(
                    tokens.span(),
                    "this imported name requires a local binding after `as`",
                )]
            })?
        };
        items.push(ImportItem { imported, local });
        if !tokens.eat_punctuator(Punctuator::Comma) {
            break;
        }
    }
    let _ = expr::expect_punctuator(tokens, Punctuator::RBrace)?;
    Ok(items)
}

/// Parse a static `import` declaration.
fn parse_import(tokens: &mut ParserTokenStream, span: Span) -> Result<Decl, Vec<Diagnostic>> {
    tokens.bump(); // `import`
                   // Bare side-effect import: `import "mod"`.
    if let TokenKind::String(src) = tokens.peek_kind().clone() {
        tokens.bump();
        let attributes = parse_with_clause(tokens)?;
        expr::consume_asi(tokens)?;
        return Ok(Decl::Import {
            span,
            spec: ImportSpec::Bare {
                request: ModuleRequest {
                    specifier: src,
                    phase: ImportPhase::Eval,
                    attributes,
                },
            },
        });
    }

    // `defer` remains a valid default binding unless the complete deferred
    // namespace prefix is present. Escapes never match the grammar terminal.
    let is_deferred_namespace = tokens.is_unescaped_ident_at(0, "defer")
        && matches!(tokens.peek2().kind, TokenKind::Punctuator(Punctuator::Mul));
    if is_deferred_namespace {
        tokens.bump();
    }

    let spec = if tokens.eat_punctuator(Punctuator::Mul) {
        // `import * as ns from "mod"`
        if !eat_unescaped_keyword(tokens, Keyword::As) {
            return Err(vec![Diagnostic::error(
                tokens.span(),
                "expected `as` after `*`",
            )]);
        }
        let ns = expr::binding_identifier(tokens).ok_or_else(|| {
            vec![Diagnostic::error(
                tokens.span(),
                "expected a namespace name",
            )]
        })?;
        ImportSpec::Namespace {
            ns,
            request: if is_deferred_namespace {
                parse_module_request_at_phase(tokens, ImportPhase::Defer)?
            } else {
                parse_module_request(tokens)?
            },
        }
    } else if matches!(
        tokens.peek_kind(),
        TokenKind::Punctuator(Punctuator::LBrace)
    ) {
        // `import { a, b } from "mod"`
        let items = parse_import_items(tokens)?;
        ImportSpec::Named {
            items,
            request: parse_module_request(tokens)?,
        }
    } else {
        // `import def from "mod"` / `import def, { a } from "mod"`
        let local = expr::binding_identifier(tokens).ok_or_else(|| {
            vec![Diagnostic::error(
                tokens.span(),
                "expected a default import name",
            )]
        })?;
        let mut namespace = None;
        let mut named = Vec::new();
        if tokens.eat_punctuator(Punctuator::Comma) {
            if tokens.eat_punctuator(Punctuator::Mul) {
                // `import def, * as ns from "mod"` — combined default+namespace.
                if !eat_unescaped_keyword(tokens, Keyword::As) {
                    return Err(vec![Diagnostic::error(
                        tokens.span(),
                        "expected `as` after `*`",
                    )]);
                }
                namespace = Some(expr::binding_identifier(tokens).ok_or_else(|| {
                    vec![Diagnostic::error(
                        tokens.span(),
                        "expected a namespace name",
                    )]
                })?);
            } else {
                named = parse_import_items(tokens)?;
            }
        }
        ImportSpec::Default {
            local,
            namespace,
            named,
            request: parse_module_request(tokens)?,
        }
    };
    expr::consume_asi(tokens)?;
    Ok(Decl::Import { span, spec })
}

/// `{ a, b as c }` export list items.
fn parse_export_items(tokens: &mut ParserTokenStream) -> Result<Vec<ExportItem>, Vec<Diagnostic>> {
    let _ = expr::expect_punctuator(tokens, Punctuator::LBrace)?;
    let mut items = Vec::new();
    while !matches!(
        tokens.peek_kind(),
        TokenKind::Punctuator(Punctuator::RBrace)
    ) {
        let local = module_export_name(tokens)?;
        let exported = if eat_unescaped_keyword(tokens, Keyword::As) {
            module_export_name(tokens)?
        } else {
            local.clone()
        };
        items.push(ExportItem { local, exported });
        if !tokens.eat_punctuator(Punctuator::Comma) {
            break;
        }
    }
    let _ = expr::expect_punctuator(tokens, Punctuator::RBrace)?;
    Ok(items)
}

/// Parse an `export` declaration.
fn parse_export(tokens: &mut ParserTokenStream, span: Span) -> Result<Decl, Vec<Diagnostic>> {
    tokens.bump(); // `export`

    // `export default …`
    if eat_unescaped_keyword(tokens, Keyword::Default) {
        // HoistableDeclaration and ClassDeclaration are declaration grammar,
        // not call/member expressions. Preserve that distinction in the AST.
        let declaration = match tokens.peek_kind().clone() {
            TokenKind::Keyword(Keyword::Function) => {
                let start = tokens.span();
                tokens.bump();
                Some(parse_function_decl(tokens, start, false, true)?)
            }
            TokenKind::Keyword(Keyword::Async) if async_function_ahead(tokens) => {
                let start = tokens.span();
                tokens.bump();
                tokens.bump();
                Some(parse_function_decl(tokens, start, true, true)?)
            }
            TokenKind::Keyword(Keyword::Class) => {
                let start = tokens.span();
                tokens.bump();
                Some(Decl::Class(Box::new(crate::class::parse_class_decl(
                    tokens,
                    start,
                    Vec::new(),
                )?)))
            }
            _ => None,
        };
        if let Some(declaration) = declaration {
            return Ok(Decl::Export {
                span,
                spec: ExportSpec::DefaultDecl(Box::new(declaration)),
            });
        }
        let value = expr::parse_assignment(tokens)?;
        expr::consume_asi(tokens)?;
        return Ok(Decl::Export {
            span,
            spec: ExportSpec::Default(value),
        });
    }

    // `export * [as ns] from "mod"`
    if tokens.eat_punctuator(Punctuator::Mul) {
        let exported = if eat_unescaped_keyword(tokens, Keyword::As) {
            Some(module_export_name(tokens)?)
        } else {
            None
        };
        let request = parse_module_request(tokens)?;
        expr::consume_asi(tokens)?;
        return Ok(Decl::Export {
            span,
            spec: ExportSpec::All { exported, request },
        });
    }

    // `export { a, b as c } [from "mod"]`
    if matches!(
        tokens.peek_kind(),
        TokenKind::Punctuator(Punctuator::LBrace)
    ) {
        let items = parse_export_items(tokens)?;
        if matches!(tokens.peek_kind(), TokenKind::Keyword(Keyword::From)) {
            let request = parse_module_request(tokens)?;
            expr::consume_asi(tokens)?;
            return Ok(Decl::Export {
                span,
                spec: ExportSpec::ReExport { items, request },
            });
        }
        expr::consume_asi(tokens)?;
        return Ok(Decl::Export {
            span,
            spec: ExportSpec::Named { items },
        });
    }

    // `export <declaration>` (var/let/const/function/class/async function)
    let inner = match tokens.peek_kind().clone() {
        TokenKind::Keyword(Keyword::Var) => {
            tokens.bump();
            parse_var(tokens, span, VarKind::Var)?
        }
        TokenKind::Keyword(Keyword::Let) => {
            tokens.bump();
            parse_var(tokens, span, VarKind::Let)?
        }
        TokenKind::Keyword(Keyword::Const) => {
            tokens.bump();
            parse_var(tokens, span, VarKind::Const)?
        }
        TokenKind::Keyword(Keyword::Function) => {
            let start = tokens.span();
            tokens.bump();
            parse_function_decl(tokens, start, false, false)?
        }
        TokenKind::Keyword(Keyword::Async) if async_function_ahead(tokens) => {
            let start = tokens.span();
            tokens.bump();
            tokens.bump();
            parse_function_decl(tokens, start, true, false)?
        }
        TokenKind::Keyword(Keyword::Class) => {
            let start = tokens.span();
            tokens.bump();
            Decl::Class(Box::new(crate::class::parse_class_decl(
                tokens,
                start,
                Vec::new(),
            )?))
        }
        other => {
            return Err(vec![Diagnostic::error(
                tokens.span(),
                format!("expected a declaration after `export`, found {:?}", other),
            )]);
        }
    };
    Ok(Decl::Export {
        span,
        spec: ExportSpec::Decl(Box::new(inner)),
    })
}

fn parse_expr_statement(
    tokens: &mut ParserTokenStream,
    span: Span,
) -> Result<Stmt, Vec<Diagnostic>> {
    let e = expr::parse_expression(tokens)?;
    expr::consume_asi(tokens)?;
    let end = tokens.span();
    let span = Span::new(span.start, end.start);
    Ok(Stmt::Expr {
        span,
        expr: Box::new(e),
    })
}

/// An expression statement that begins with a keyword the lexer tagged (e.g.
/// `true`, `this`). We reconstruct an identifier-like primary where possible.
fn parse_expr_statement_with_prefix_keyword(
    tokens: &mut ParserTokenStream,
    kw: Keyword,
    span: Span,
) -> Result<Stmt, Vec<Diagnostic>> {
    let primary = match kw {
        Keyword::This => Expr::This(span),
        Keyword::True => Expr::Lit(Lit::Boolean(span, true)),
        Keyword::False => Expr::Lit(Lit::Boolean(span, false)),
        Keyword::Null => Expr::Lit(Lit::Null(span)),
        _ => {
            return Err(vec![Diagnostic::error(
                span,
                format!("unexpected keyword {:?}", kw),
            )]);
        }
    };
    expr::consume_asi(tokens)?;
    let end = tokens.span();
    Ok(Stmt::Expr {
        span: Span::new(span.start, end.start),
        expr: Box::new(primary),
    })
}

fn parse_var(
    tokens: &mut ParserTokenStream,
    start: Span,
    kind: VarKind,
) -> Result<Decl, Vec<Diagnostic>> {
    let mut declarations = Vec::new();
    loop {
        let name = expr::parse_binding_target(tokens)?;
        if matches!(kind, VarKind::Using | VarKind::AwaitUsing)
            && !matches!(name, Pat::Ident { .. })
        {
            return Err(vec![Diagnostic::error(
                name.span(),
                "using declarations require a binding identifier",
            )]);
        }
        let name_span = name.span();
        let init = if tokens.eat_punctuator(Punctuator::Assign) {
            Some(expr::parse_assignment(tokens)?)
        } else {
            None
        };
        let decl_span = Span::new(name_span.start, tokens.span().start);
        declarations.push(VarDeclarator {
            span: decl_span,
            name,
            init,
        });
        if !tokens.eat_punctuator(Punctuator::Comma) {
            break;
        }
    }
    expr::consume_asi(tokens)?;
    let end = tokens.span();
    Ok(Decl::Var {
        span: Span::new(start.start, end.start),
        kind,
        declarations,
    })
}

fn parse_return(tokens: &mut ParserTokenStream, start: Span) -> Result<Stmt, Vec<Diagnostic>> {
    if !tokens.in_function() {
        return Err(vec![Diagnostic::error(
            start,
            "a return statement is only allowed inside a function body",
        )]);
    }
    // Restricted production: `return [no LineTerminator here] Expression? ;`.
    let arg = if matches!(
        tokens.peek_kind(),
        TokenKind::Punctuator(Punctuator::Semicolon)
            | TokenKind::Punctuator(Punctuator::RBrace)
            | TokenKind::Eof
    ) || tokens.preceded_by_newline()
    {
        None
    } else {
        Some(Box::new(expr::parse_expression(tokens)?))
    };
    expr::consume_asi(tokens)?;
    let end = tokens.span();
    Ok(Stmt::Return {
        span: Span::new(start.start, end.start),
        arg,
    })
}

fn parse_throw(tokens: &mut ParserTokenStream, start: Span) -> Result<Stmt, Vec<Diagnostic>> {
    // Unlike `return`, `throw` has no operand-less production. A line
    // terminator before its required Expression is therefore always a syntax
    // error; ASI cannot turn it into a valid `throw;` statement.
    if tokens.preceded_by_newline() {
        return Err(vec![Diagnostic::error(
            start,
            "a line terminator is not allowed after `throw`",
        )]);
    }
    let arg = expr::parse_expression(tokens)?;
    expr::consume_asi(tokens)?;
    Ok(Stmt::Throw {
        span: Span::new(start.start, arg.span().end),
        arg: Box::new(arg),
    })
}

fn parse_function_decl(
    tokens: &mut ParserTokenStream,
    start: Span,
    is_async: bool,
    allow_anonymous: bool,
) -> Result<Decl, Vec<Diagnostic>> {
    let is_generator = tokens.eat_punctuator(Punctuator::Mul);
    let name = match expr::binding_identifier(tokens) {
        Some(n) => Some(n),
        None if !allow_anonymous => {
            return Err(vec![Diagnostic::error(
                tokens.span(),
                format!("expected function name, found {:?}", tokens.peek_kind()),
            )]);
        }
        None => None,
    };
    // Params + body are parsed in this function's async/generator context.
    tokens.enter_fn(is_async, is_generator);
    let result = (|| {
        let params = expr::parse_params(tokens)?;
        let (body, close) = expr::parse_block(tokens)?;
        Ok(Decl::Function(Box::new(FunctionDecl {
            span: Span::new(start.start, close.end),
            name,
            params,
            body,
            is_async,
            is_generator,
        })))
    })();
    tokens.pop_ctx();
    result
}

fn parse_if(tokens: &mut ParserTokenStream, start: Span) -> Result<Stmt, Vec<Diagnostic>> {
    let _ = expr::expect_punctuator(tokens, Punctuator::LParen)?;
    let test = expr::parse_expression(tokens)?;
    let _ = expr::expect_punctuator(tokens, Punctuator::RParen)?;
    let cons = Box::new(parse_statement(tokens)?);
    let alt = if matches!(tokens.peek_kind(), TokenKind::Keyword(Keyword::Else)) {
        tokens.bump();
        Some(Box::new(parse_statement(tokens)?))
    } else {
        None
    };
    let end = tokens.span();
    Ok(Stmt::If {
        span: Span::new(start.start, end.start),
        test: Box::new(test),
        cons,
        alt,
    })
}

fn parse_while(tokens: &mut ParserTokenStream, start: Span) -> Result<Stmt, Vec<Diagnostic>> {
    let _ = expr::expect_punctuator(tokens, Punctuator::LParen)?;
    let test = expr::parse_expression(tokens)?;
    let _ = expr::expect_punctuator(tokens, Punctuator::RParen)?;
    let body = Box::new(parse_statement(tokens)?);
    let end = tokens.span();
    Ok(Stmt::While {
        span: Span::new(start.start, end.start),
        test: Box::new(test),
        body,
    })
}

fn parse_do_while(tokens: &mut ParserTokenStream, start: Span) -> Result<Stmt, Vec<Diagnostic>> {
    let body = Box::new(parse_statement(tokens)?);
    // `while` is required.
    let _ = match tokens.peek_kind() {
        TokenKind::Keyword(Keyword::While) => tokens.bump(),
        other => {
            return Err(vec![Diagnostic::error(
                tokens.span(),
                format!("expected `while`, found {:?}", other),
            )]);
        }
    };
    let _ = expr::expect_punctuator(tokens, Punctuator::LParen)?;
    let test = expr::parse_expression(tokens)?;
    let _ = expr::expect_punctuator(tokens, Punctuator::RParen)?;
    // Optional trailing `;` (ASI also applies).
    let _ = tokens.eat_punctuator(Punctuator::Semicolon);
    let end = tokens.span();
    Ok(Stmt::DoWhile {
        span: Span::new(start.start, end.start),
        body,
        test: Box::new(test),
    })
}

/// `for (` — C-style, for-in, or for-of.
fn parse_for(tokens: &mut ParserTokenStream, start: Span) -> Result<Stmt, Vec<Diagnostic>> {
    // `for await ( ... of ... )` — only valid inside async functions, and only
    // with a `for...of` body (not for-in / C-style). The `await` is recognized
    // positionally here rather than via context tracking.
    let for_await = if matches!(tokens.peek_kind(), TokenKind::Keyword(Keyword::Await)) {
        tokens.bump();
        true
    } else {
        false
    };

    let _ = expr::expect_punctuator(tokens, Punctuator::LParen)?;

    // Parse the init/left-hand side.
    let init_decl_kind = match tokens.peek_kind() {
        TokenKind::Keyword(Keyword::Var) => Some(VarKind::Var),
        // `let` is a declaration only when a binding pattern follows
        // (`let {`, `let [`, `let ident`); otherwise `let` is a plain
        // identifier reference (sloppy mode: `for (let in obj)`, `for (let = 1;;)`).
        TokenKind::Keyword(Keyword::Let) if let_is_declaration_ahead(tokens, true) => {
            Some(VarKind::Let)
        }
        TokenKind::Keyword(Keyword::Const) => Some(VarKind::Const),
        _ => using_decl_kind(tokens),
    };

    if let Some(kind) = init_decl_kind {
        // `using`/`await using` carry a leading contextual keyword to consume.
        if matches!(kind, VarKind::Using | VarKind::AwaitUsing) {
            if matches!(kind, VarKind::AwaitUsing) {
                tokens.bump(); // `await`
            }
            tokens.bump(); // `using`
        } else {
            tokens.bump();
        }
        // A binding *target* (no default) — the `= init` belongs to the
        // declarator, not the pattern. (`parse_binding_pattern` would wrongly
        // swallow `= 0` as a default and leave the binding uninitialized.)
        let mut pat = expr::parse_binding_target(tokens)?;
        // for-in / for-of: `pat of expr` / `pat in expr`.
        if matches!(tokens.peek_kind(), TokenKind::Keyword(Keyword::Of)) {
            if tokens.current_token_contains_escape() {
                return Err(vec![Diagnostic::error(
                    tokens.span(),
                    "the `of` terminal may not contain an escape sequence",
                )]);
            }
            tokens.bump();
            let right = Box::new(expr::parse_assignment(tokens)?);
            let _ = expr::expect_punctuator(tokens, Punctuator::RParen)?;
            let body = Box::new(parse_statement(tokens)?);
            let end = tokens.span();
            return Ok(Stmt::ForOf {
                span: Span::new(start.start, end.start),
                left: ForTarget::Var(Box::new(make_var_decl(kind, pat, None))),
                right,
                body,
                is_async: for_await,
            });
        }
        // `for await` permits only a `for...of` body.
        if for_await {
            return Err(vec![Diagnostic::error(
                tokens.span(),
                "`for await` requires a `for...of` body",
            )]);
        }
        if matches!(tokens.peek_kind(), TokenKind::Keyword(Keyword::In)) {
            tokens.bump();
            // `for (x in Expression)` — the RHS is a full Expression[+In]
            // (commas / sequence allowed), not a single AssignmentExpression.
            let right = Box::new(expr::parse_expression(tokens)?);
            let _ = expr::expect_punctuator(tokens, Punctuator::RParen)?;
            let body = Box::new(parse_statement(tokens)?);
            let end = tokens.span();
            return Ok(Stmt::ForIn {
                span: Span::new(start.start, end.start),
                left: ForTarget::Var(Box::new(make_var_decl(kind, pat, None))),
                right,
                body,
            });
        }
        // C-style with a declaration init: `pat [= init] [, pat2 [= init2]]* ;`
        let mut declarators = Vec::new();
        loop {
            let pat_span = pat.span();
            let vinit = if tokens.eat_punctuator(Punctuator::Assign) {
                Some(expr::parse_assignment(tokens)?)
            } else {
                None
            };
            let decl_span = Span::new(pat_span.start, tokens.span().start);
            declarators.push(VarDeclarator {
                span: decl_span,
                name: pat.clone(),
                init: vinit,
            });
            if !tokens.eat_punctuator(Punctuator::Comma) {
                break;
            }
            // Subsequent declarators reuse the full binding-target grammar.
            pat = expr::parse_binding_target(tokens)?;
        }
        let decl = Decl::Var {
            span: Span::new(start.start, tokens.span().start),
            kind,
            declarations: declarators,
        };
        return finish_c_for(tokens, start, Some(ForInit::Var(Box::new(decl))));
    }

    // No declaration: empty init (`;`), an expression init (C-style), or a
    // for-in/of with an expression LHS.
    if matches!(
        tokens.peek_kind(),
        TokenKind::Punctuator(Punctuator::Semicolon)
    ) {
        return finish_c_for(tokens, start, None);
    }
    let lhs = expr::parse_expression_inner(tokens, false)?;
    if matches!(tokens.peek_kind(), TokenKind::Keyword(Keyword::Of)) {
        if !for_await
            && matches!(
            &lhs,
            Expr::Ident { span, name }
                if name == "async" && !tokens.token_span_contains_escape(*span)
            )
        {
            return Err(vec![Diagnostic::error(
                lhs.span(),
                "an unparenthesized `async` may not precede `of` in a for-of statement",
            )]);
        }
        if tokens.current_token_contains_escape() {
            return Err(vec![Diagnostic::error(
                tokens.span(),
                "the `of` terminal may not contain an escape sequence",
            )]);
        }
        tokens.bump();
        let right = Box::new(expr::parse_assignment(tokens)?);
        let _ = expr::expect_punctuator(tokens, Punctuator::RParen)?;
        let body = Box::new(parse_statement(tokens)?);
        let end = tokens.span();
        return Ok(Stmt::ForOf {
            span: Span::new(start.start, end.start),
            left: ForTarget::Pat(
                expr::assignment_pattern_from_expr(&lhs).map_err(|error| vec![error])?,
            ),
            right,
            body,
            is_async: for_await,
        });
    }
    // `for await` permits only a `for...of` body.
    if for_await {
        return Err(vec![Diagnostic::error(
            tokens.span(),
            "`for await` requires a `for...of` body",
        )]);
    }
    if matches!(tokens.peek_kind(), TokenKind::Keyword(Keyword::In)) {
        tokens.bump();
        // for-in RHS is a full Expression[+In] (sequence / commas allowed).
        let right = Box::new(expr::parse_expression(tokens)?);
        let _ = expr::expect_punctuator(tokens, Punctuator::RParen)?;
        let body = Box::new(parse_statement(tokens)?);
        let end = tokens.span();
        return Ok(Stmt::ForIn {
            span: Span::new(start.start, end.start),
            left: ForTarget::Pat(
                expr::assignment_pattern_from_expr(&lhs).map_err(|error| vec![error])?,
            ),
            right,
            body,
        });
    }
    finish_c_for(tokens, start, Some(ForInit::Expr(lhs)))
}

fn finish_c_for(
    tokens: &mut ParserTokenStream,
    start: Span,
    init: Option<ForInit>,
) -> Result<Stmt, Vec<Diagnostic>> {
    let _ = expr::expect_punctuator(tokens, Punctuator::Semicolon)?;
    let test = if matches!(
        tokens.peek_kind(),
        TokenKind::Punctuator(Punctuator::Semicolon)
    ) {
        None
    } else {
        Some(Box::new(expr::parse_expression(tokens)?))
    };
    let _ = expr::expect_punctuator(tokens, Punctuator::Semicolon)?;
    let update = if matches!(
        tokens.peek_kind(),
        TokenKind::Punctuator(Punctuator::RParen)
    ) {
        None
    } else {
        Some(Box::new(expr::parse_expression(tokens)?))
    };
    let _ = expr::expect_punctuator(tokens, Punctuator::RParen)?;
    let body = Box::new(parse_statement(tokens)?);
    let end = tokens.span();
    Ok(Stmt::For {
        span: Span::new(start.start, end.start),
        init,
        test,
        update,
        body,
    })
}

fn make_var_decl(kind: VarKind, name: Pat, init: Option<Expr>) -> Decl {
    let span = name.span();
    Decl::Var {
        span,
        kind,
        declarations: vec![VarDeclarator { span, name, init }],
    }
}

fn parse_switch(tokens: &mut ParserTokenStream, start: Span) -> Result<Stmt, Vec<Diagnostic>> {
    let _ = expr::expect_punctuator(tokens, Punctuator::LParen)?;
    let disc = expr::parse_expression(tokens)?;
    let _ = expr::expect_punctuator(tokens, Punctuator::RParen)?;
    let _ = expr::expect_punctuator(tokens, Punctuator::LBrace)?;
    let mut cases = Vec::new();
    while !matches!(
        tokens.peek_kind(),
        TokenKind::Punctuator(Punctuator::RBrace) | TokenKind::Eof
    ) {
        let case_span = tokens.span();
        let test = if tokens.eat_keyword(Keyword::Case) {
            Some(expr::parse_expression(tokens)?)
        } else if tokens.eat_keyword(Keyword::Default) {
            None
        } else {
            return Err(vec![Diagnostic::error(
                tokens.span(),
                "expected `case` or `default` in switch body",
            )]);
        };
        let _ = expr::expect_punctuator(tokens, Punctuator::Colon)?;
        let mut body = Vec::new();
        while !matches!(
            tokens.peek_kind(),
            TokenKind::Punctuator(Punctuator::RBrace)
                | TokenKind::Keyword(Keyword::Case)
                | TokenKind::Keyword(Keyword::Default)
                | TokenKind::Eof
        ) {
            match parse_statement_list_item(tokens) {
                Ok(s) => body.push(s),
                Err(e) => {
                    expr::recover_to_statement_boundary(tokens);
                    return Err(e);
                }
            }
        }
        cases.push(SwitchCase {
            span: case_span,
            test,
            body,
        });
    }
    let close = expr::expect_punctuator(tokens, Punctuator::RBrace)?;
    Ok(Stmt::Switch {
        span: Span::new(start.start, close.end),
        disc: Box::new(disc),
        cases,
    })
}

fn parse_try(tokens: &mut ParserTokenStream, start: Span) -> Result<Stmt, Vec<Diagnostic>> {
    let (block_body, close) = expr::parse_block(tokens)?;
    let block = TryBlock {
        span: Span::new(start.start, close.end),
        body: block_body,
    };
    let handler = if tokens.eat_keyword(Keyword::Catch) {
        let catch_start = close.end;
        let param = if tokens.eat_punctuator(Punctuator::LParen) {
            let p = expr::parse_binding_pattern(tokens)?;
            let _ = expr::expect_punctuator(tokens, Punctuator::RParen)?;
            Some(p)
        } else {
            None
        };
        let (body, cclose) = expr::parse_block(tokens)?;
        Some(Box::new(CatchClause {
            span: Span::new(catch_start, cclose.end),
            param,
            body,
        }))
    } else {
        None
    };
    let finalizer = if tokens.eat_keyword(Keyword::Finally) {
        let (body, _) = expr::parse_block(tokens)?;
        Some(body)
    } else {
        None
    };
    let end = tokens.span();
    Ok(Stmt::Try {
        span: Span::new(start.start, end.start),
        block: Box::new(block),
        handler,
        finalizer,
    })
}

fn parse_with(tokens: &mut ParserTokenStream, start: Span) -> Result<Stmt, Vec<Diagnostic>> {
    let _ = expr::expect_punctuator(tokens, Punctuator::LParen)?;
    let obj = expr::parse_expression(tokens)?;
    let _ = expr::expect_punctuator(tokens, Punctuator::RParen)?;
    let body = Box::new(parse_statement(tokens)?);
    let end = tokens.span();
    Ok(Stmt::With {
        span: Span::new(start.start, end.start),
        obj: Box::new(obj),
        body,
    })
}

fn optional_label(tokens: &mut ParserTokenStream) -> Option<String> {
    // Restricted production: `continue`/`break` may only take a label when no
    // line terminator separates them (`[no LineTerminator here]`). Otherwise ASI
    // inserts `;` first and the identifier begins the next statement.
    if tokens.preceded_by_newline() {
        return None;
    }
    if let TokenKind::Ident(n) = tokens.peek_kind().clone() {
        tokens.bump();
        Some(n)
    } else {
        None
    }
}

/// The statement-parsing handle used by the top-level driver.
pub struct StmtParser<'a> {
    tokens: &'a mut ParserTokenStream,
}

impl<'a> StmtParser<'a> {
    pub fn new(tokens: &'a mut ParserTokenStream) -> StmtParser<'a> {
        StmtParser { tokens }
    }

    pub fn is_eof(&mut self) -> bool {
        self.tokens.is_eof()
    }

    pub fn span(&self) -> Span {
        self.tokens.span()
    }

    pub fn parse_program_item(&mut self) -> Result<ProgramItem, Vec<Diagnostic>> {
        parse_program_item(self.tokens)
    }

    pub fn recover_to_statement_boundary(&mut self) {
        if self.tokens.is_eof() {
            return;
        }
        expr::recover_to_statement_boundary(self.tokens);
    }
}
