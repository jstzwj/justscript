//! Statement parsing.
//!
//! Statement-level grammar is driven by [`parse_statement`] / dispatch on the
//! current keyword. Expression statements fall through to [`crate::expr`].

use crate::expr;
use crate::token_stream::ParserTokenStream;
use js_diagnostics::Diagnostic;
use js_syntax::ast::expr::Expr;
use js_syntax::ast::lit::Lit;
use js_syntax::ast::pat::Pat;
use js_syntax::ast::stmt::{
    CatchClause, Decl, ExportItem, ExportSpec, ForInit, ForTarget, ImportItem, ImportSpec, Stmt,
    SwitchCase, TryBlock, VarDeclarator, VarKind,
};
use js_syntax::ast::{FunctionDecl, ProgramItem};
use js_syntax::keyword::Keyword;
use js_syntax::punctuator::Punctuator;
use js_syntax::source::Span;
use js_syntax::token::TokenKind;

/// Parse one statement.
pub fn parse_statement(tokens: &mut ParserTokenStream) -> Result<Stmt, Vec<Diagnostic>> {
    let span = tokens.span();

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
        // Labeled statement: `ident : stmt`. Detect only when the *next* token
        // is a colon (so e.g. `a: 1` as an object literal isn't misread).
        if kw_ppeks_colon(tokens) {
            let label = match tokens.bump().kind {
                TokenKind::Ident(n) => n,
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

        // `async function` declaration: hoisted like a function declaration.
        if matches!(kw, Keyword::Async)
            && matches!(tokens.peek2().kind, TokenKind::Keyword(Keyword::Function))
        {
            tokens.bump(); // `async`
            tokens.bump(); // `function`
            let decl = parse_function_decl(tokens, span, true)?;
            return Ok(Stmt::Decl(Box::new(decl)));
        }
        // Only consume the keyword for actual statement-leading keywords.
        if !is_statement_keyword(kw) {
            return parse_expr_statement(tokens, span);
        }
        tokens.bump();
        let stmt = match kw {
            Keyword::Var => Stmt::Decl(Box::new(parse_var(tokens, span, VarKind::Var)?)),
            Keyword::Let => Stmt::Decl(Box::new(parse_var(tokens, span, VarKind::Let)?)),
            Keyword::Const => Stmt::Decl(Box::new(parse_var(tokens, span, VarKind::Const)?)),
            Keyword::Return => parse_return(tokens, span)?,
            Keyword::Function => Stmt::Decl(Box::new(parse_function_decl(tokens, span, false)?)),
            Keyword::Class => Stmt::Decl(Box::new(Decl::Class(Box::new(
                crate::class::parse_class_decl(tokens, span, Vec::new())?,
            )))),
            Keyword::If => parse_if(tokens, span)?,
            Keyword::While => parse_while(tokens, span)?,
            Keyword::Do => parse_do_while(tokens, span)?,
            Keyword::For => parse_for(tokens, span)?,
            Keyword::Switch => parse_switch(tokens, span)?,
            Keyword::Try => parse_try(tokens, span)?,
            Keyword::Throw => {
                let arg = expr::parse_expression(tokens)?;
                expr::consume_asi(tokens)?;
                Stmt::Throw {
                    span,
                    arg: Box::new(arg),
                }
            }
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

/// True when the current token is an identifier immediately followed by `:`.
fn kw_ppeks_colon(tokens: &ParserTokenStream) -> bool {
    if !matches!(tokens.peek_kind(), TokenKind::Ident(_)) {
        return false;
    }
    matches!(tokens.peek2().kind, TokenKind::Punctuator(Punctuator::Colon))
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
                    let decl = parse_function_decl(tokens, span, false)?;
                    return Ok(ProgramItem::Decl(decl));
                }
                let c = crate::class::parse_class_decl(tokens, span, Vec::new())?;
                return Ok(ProgramItem::Decl(Decl::Class(Box::new(c))));
            }
            Keyword::Async
                if matches!(tokens.peek2().kind, TokenKind::Keyword(Keyword::Function)) =>
            {
                let span = tokens.span();
                tokens.bump(); // async
                tokens.bump(); // function
                let decl = parse_function_decl(tokens, span, true)?;
                return Ok(ProgramItem::Decl(decl));
            }
            Keyword::Var | Keyword::Let | Keyword::Const => {
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
                // Could be a dynamic import call `import(...)` (expression) or
                // a static import declaration. Only static is a ProgramItem.
                if matches!(
                    tokens.peek2().kind,
                    TokenKind::Punctuator(Punctuator::LParen)
                ) {
                    // dynamic import — fall through to statement parsing.
                } else {
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

/// `from "module"` — consume the `from` keyword and the module specifier string.
fn parse_module_source(tokens: &mut ParserTokenStream) -> Result<String, Vec<Diagnostic>> {
    if !tokens.eat_keyword(Keyword::From) {
        return Err(vec![Diagnostic::error(
            tokens.span(),
            "expected `from` in import/export",
        )]);
    }
    match tokens.peek_kind().clone() {
        TokenKind::String(src) => {
            tokens.bump();
            Ok(src)
        }
        other => Err(vec![Diagnostic::error(
            tokens.span(),
            format!("expected a module specifier string, found {:?}", other),
        )]),
    }
}

/// `{ a, b as c, ... }` import bindings.
fn parse_import_items(tokens: &mut ParserTokenStream) -> Result<Vec<ImportItem>, Vec<Diagnostic>> {
    let _ = expr::expect_punctuator(tokens, Punctuator::LBrace)?;
    let mut items = Vec::new();
    while !matches!(tokens.peek_kind(), TokenKind::Punctuator(Punctuator::RBrace)) {
        let imported = ident_name(tokens)?;
        let local = if tokens.eat_keyword(Keyword::As) {
            expr::binding_identifier(tokens).ok_or_else(|| {
                vec![Diagnostic::error(tokens.span(), "expected a binding name after `as`")]
            })?
        } else {
            imported.clone()
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
fn parse_import(
    tokens: &mut ParserTokenStream,
    span: Span,
) -> Result<Decl, Vec<Diagnostic>> {
    tokens.bump(); // `import`
    // Bare side-effect import: `import "mod"`.
    if let TokenKind::String(src) = tokens.peek_kind().clone() {
        tokens.bump();
        expr::consume_asi(tokens)?;
        return Ok(Decl::Import {
            span,
            spec: ImportSpec::Bare { source: src },
        });
    }

    let spec = if tokens.eat_punctuator(Punctuator::Mul) {
        // `import * as ns from "mod"`
        if !tokens.eat_keyword(Keyword::As) {
            return Err(vec![Diagnostic::error(tokens.span(), "expected `as` after `*`")]);
        }
        let ns = expr::binding_identifier(tokens).ok_or_else(|| {
            vec![Diagnostic::error(tokens.span(), "expected a namespace name")]
        })?;
        ImportSpec::Namespace {
            ns,
            source: parse_module_source(tokens)?,
        }
    } else if matches!(tokens.peek_kind(), TokenKind::Punctuator(Punctuator::LBrace)) {
        // `import { a, b } from "mod"`
        let items = parse_import_items(tokens)?;
        ImportSpec::Named {
            items,
            source: parse_module_source(tokens)?,
        }
    } else {
        // `import def from "mod"` / `import def, { a } from "mod"`
        let local = expr::binding_identifier(tokens).ok_or_else(|| {
            vec![Diagnostic::error(tokens.span(), "expected a default import name")]
        })?;
        let mut named = Vec::new();
        if tokens.eat_punctuator(Punctuator::Comma) {
            if tokens.eat_punctuator(Punctuator::Mul) {
                // `import def, * as ns from "mod"` — combined default+namespace.
                // The AST's `Default` spec only carries named items; fall back to
                // a Namespace spec (dropping the default binding) to stay valid.
                let _ = tokens.eat_keyword(Keyword::As);
                let _ = expr::binding_identifier(tokens);
                return Ok(Decl::Import {
                    span,
                    spec: ImportSpec::Namespace {
                        ns: String::new(),
                        source: parse_module_source(tokens)?,
                    },
                });
            }
            named = parse_import_items(tokens)?;
        }
        ImportSpec::Default {
            local,
            named,
            source: parse_module_source(tokens)?,
        }
    };
    expr::consume_asi(tokens)?;
    Ok(Decl::Import { span, spec })
}

/// `{ a, b as c }` export list items.
fn parse_export_items(tokens: &mut ParserTokenStream) -> Result<Vec<ExportItem>, Vec<Diagnostic>> {
    let _ = expr::expect_punctuator(tokens, Punctuator::LBrace)?;
    let mut items = Vec::new();
    while !matches!(tokens.peek_kind(), TokenKind::Punctuator(Punctuator::RBrace)) {
        let local = ident_name(tokens)?;
        let exported = if tokens.eat_keyword(Keyword::As) {
            ident_name(tokens)?
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
fn parse_export(
    tokens: &mut ParserTokenStream,
    span: Span,
) -> Result<Decl, Vec<Diagnostic>> {
    tokens.bump(); // `export`

    // `export default …`
    if tokens.eat_keyword(Keyword::Default) {
        // `export default function/class` (named or anonymous) or an expression.
        let value = expr::parse_assignment(tokens)?;
        expr::consume_asi(tokens)?;
        return Ok(Decl::Export {
            span,
            spec: ExportSpec::Default(value),
        });
    }

    // `export * [as ns] from "mod"`
    if tokens.eat_punctuator(Punctuator::Mul) {
        if tokens.eat_keyword(Keyword::As) {
            // `export * as ns from "mod"` — namespace re-export. The AST has no
            // dedicated variant; record as All (the `as ns` is dropped).
            let _ = ident_name(tokens)?;
        }
        let source = parse_module_source(tokens)?;
        expr::consume_asi(tokens)?;
        return Ok(Decl::Export {
            span,
            spec: ExportSpec::All { source },
        });
    }

    // `export { a, b as c } [from "mod"]`
    if matches!(tokens.peek_kind(), TokenKind::Punctuator(Punctuator::LBrace)) {
        let items = parse_export_items(tokens)?;
        if matches!(tokens.peek_kind(), TokenKind::Keyword(Keyword::From)) {
            let source = parse_module_source(tokens)?;
            expr::consume_asi(tokens)?;
            return Ok(Decl::Export {
                span,
                spec: ExportSpec::ReExport { items, source },
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
            tokens.bump();
            parse_function_decl(tokens, span, false)?
        }
        TokenKind::Keyword(Keyword::Async)
            if matches!(tokens.peek2().kind, TokenKind::Keyword(Keyword::Function)) =>
        {
            tokens.bump();
            tokens.bump();
            parse_function_decl(tokens, span, true)?
        }
        TokenKind::Keyword(Keyword::Class) => {
            tokens.bump();
            Decl::Class(Box::new(crate::class::parse_class_decl(tokens, span, Vec::new())?))
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

fn parse_function_decl(
    tokens: &mut ParserTokenStream,
    start: Span,
    is_async: bool,
) -> Result<Decl, Vec<Diagnostic>> {
    let is_generator = tokens.eat_punctuator(Punctuator::Mul);
    let name = match expr::binding_identifier(tokens) {
        Some(n) => Some(n),
        None => {
            return Err(vec![Diagnostic::error(
                tokens.span(),
                format!("expected function name, found {:?}", tokens.peek_kind()),
            )]);
        }
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
        TokenKind::Keyword(Keyword::Let) => Some(VarKind::Let),
        TokenKind::Keyword(Keyword::Const) => Some(VarKind::Const),
        _ => None,
    };

    if let Some(kind) = init_decl_kind {
        tokens.bump();
        // A binding *target* (no default) — the `= init` belongs to the
        // declarator, not the pattern. (`parse_binding_pattern` would wrongly
        // swallow `= 0` as a default and leave the binding uninitialized.)
        let pat = expr::parse_binding_target(tokens)?;
        // for-in / for-of: `pat of expr` / `pat in expr`.
        if matches!(tokens.peek_kind(), TokenKind::Keyword(Keyword::Of)) {
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
            let right = Box::new(expr::parse_assignment(tokens)?);
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
        // C-style with a declaration init: optional `= init`, then `;`.
        let vinit = if tokens.eat_punctuator(Punctuator::Assign) {
            Some(expr::parse_assignment(tokens)?)
        } else {
            None
        };
        let decl = make_var_decl(kind, pat, vinit);
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
    let lhs = expr::parse_expression(tokens)?;
    if matches!(tokens.peek_kind(), TokenKind::Keyword(Keyword::Of)) {
        tokens.bump();
        let right = Box::new(expr::parse_assignment(tokens)?);
        let _ = expr::expect_punctuator(tokens, Punctuator::RParen)?;
        let body = Box::new(parse_statement(tokens)?);
        let end = tokens.span();
        return Ok(Stmt::ForOf {
            span: Span::new(start.start, end.start),
            left: ForTarget::Pat(pat_from_expr_or_identity(&lhs)),
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
        let right = Box::new(expr::parse_assignment(tokens)?);
        let _ = expr::expect_punctuator(tokens, Punctuator::RParen)?;
        let body = Box::new(parse_statement(tokens)?);
        let end = tokens.span();
        return Ok(Stmt::ForIn {
            span: Span::new(start.start, end.start),
            left: ForTarget::Pat(pat_from_expr_or_identity(&lhs)),
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
        declarations: vec![VarDeclarator {
            span,
            name,
            init,
        }],
    }
}

/// A for-in/of target that is a plain expression LHS — represent as an ident
/// pattern when possible, otherwise a synthetic placeholder pattern.
fn pat_from_expr_or_identity(e: &Expr) -> Pat {
    match e {
        Expr::Ident { span, name } => Pat::Ident {
            span: *span,
            name: name.clone(),
        },
        _ => Pat::Ident {
            span: e.span(),
            name: String::new(),
        },
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
            match parse_statement(tokens) {
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
