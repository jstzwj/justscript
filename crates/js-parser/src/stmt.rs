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
use js_syntax::ast::stmt::{Decl, ForInit, Stmt, VarDeclarator, VarKind};
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

    // Keyword-led statements.
    if let TokenKind::Keyword(kw) = tokens.peek_kind().clone() {
        tokens.bump();
        let stmt = match kw {
            Keyword::Var => Stmt::Decl(Box::new(parse_var(tokens, span, VarKind::Var)?)),
            Keyword::Let => Stmt::Decl(Box::new(parse_var(tokens, span, VarKind::Let)?)),
            Keyword::Const => Stmt::Decl(Box::new(parse_var(tokens, span, VarKind::Const)?)),
            Keyword::Return => parse_return(tokens, span)?,
            Keyword::Function => Stmt::Decl(Box::new(parse_function_decl(tokens, span)?)),
            Keyword::If => parse_if(tokens, span)?,
            Keyword::While => parse_while(tokens, span)?,
            Keyword::Break => {
                let label = optional_label(tokens);
                consume_semicolon(tokens);
                Stmt::Break { span, label }
            }
            Keyword::Continue => {
                let label = optional_label(tokens);
                consume_semicolon(tokens);
                Stmt::Continue { span, label }
            }
            Keyword::Debugger => {
                consume_semicolon(tokens);
                Stmt::Debugger(span)
            }
            Keyword::Throw => {
                let arg = expr::parse_expression(tokens)?;
                consume_semicolon(tokens);
                Stmt::Throw {
                    span,
                    arg: Box::new(arg),
                }
            }
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

/// Parse a top-level [`ProgramItem`].
pub fn parse_program_item(tokens: &mut ParserTokenStream) -> Result<ProgramItem, Vec<Diagnostic>> {
    // Hoisted declarations become ProgramItem::Decl; everything else is a Stmt.
    if let TokenKind::Keyword(kw) = tokens.peek_kind().clone() {
        match kw {
            Keyword::Function | Keyword::Class => {
                let span = tokens.span();
                tokens.bump();
                if kw == Keyword::Function {
                    let decl = parse_function_decl(tokens, span)?;
                    return Ok(ProgramItem::Decl(decl));
                }
                // class: not implemented for milestone 1; fall through to error.
                return Err(vec![Diagnostic::error(span, "class declarations not implemented yet")]);
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
            _ => {}
        }
    }
    let stmt = parse_statement(tokens)?;
    Ok(ProgramItem::Stmt(stmt))
}

fn parse_expr_statement(
    tokens: &mut ParserTokenStream,
    span: Span,
) -> Result<Stmt, Vec<Diagnostic>> {
    let e = expr::parse_expression(tokens)?;
    consume_semicolon(tokens);
    let end = tokens.span();
    let span = Span::new(span.start, end.start);
    Ok(Stmt::Expr {
        span,
        expr: Box::new(e),
    })
}

/// An expression statement that begins with a keyword the lexer tagged (e.g.
/// `true`, `this`). We reconstruct an identifier-like primary by deferring to
/// the expression parser via a synthetic rewind — simplest is to parse the
/// keyword as a literal where possible.
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
    // Continue parsing as a postfix/binary continuation is not needed for the
    // milestone; wrap as a parenthesized-equivalent expression statement.
    consume_semicolon(tokens);
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
        let name_span = tokens.span();
        let name = match tokens.peek_kind().clone() {
            TokenKind::Ident(n) => {
                tokens.bump();
                n
            }
            other => {
                return Err(vec![Diagnostic::error(
                    name_span,
                    format!("expected variable name, found {:?}", other),
                )]);
            }
        };
        let init = if tokens.eat_punctuator(Punctuator::Assign) {
            Some(expr::parse_assignment(tokens)?)
        } else {
            None
        };
        let decl_span = Span::new(name_span.start, tokens.span().start);
        declarations.push(VarDeclarator {
            span: decl_span,
            name: Pat::Ident {
                span: name_span,
                name,
            },
            init,
        });
        if !tokens.eat_punctuator(Punctuator::Comma) {
            break;
        }
    }
    consume_semicolon(tokens);
    let end = tokens.span();
    Ok(Decl::Var {
        span: Span::new(start.start, end.start),
        kind,
        declarations,
    })
}

fn parse_return(tokens: &mut ParserTokenStream, start: Span) -> Result<Stmt, Vec<Diagnostic>> {
    // `return;` or `return expr;` (ASI-friendly: also accept EOF / `}`).
    let arg = if matches!(
        tokens.peek_kind(),
        TokenKind::Punctuator(Punctuator::Semicolon)
            | TokenKind::Punctuator(Punctuator::RBrace)
            | TokenKind::Eof
    ) {
        None
    } else {
        Some(Box::new(expr::parse_expression(tokens)?))
    };
    consume_semicolon(tokens);
    let end = tokens.span();
    Ok(Stmt::Return {
        span: Span::new(start.start, end.start),
        arg,
    })
}

fn parse_function_decl(
    tokens: &mut ParserTokenStream,
    start: Span,
) -> Result<Decl, Vec<Diagnostic>> {
    let name = match tokens.peek_kind().clone() {
        TokenKind::Ident(n) => {
            tokens.bump();
            n
        }
        other => {
            return Err(vec![Diagnostic::error(
                tokens.span(),
                format!("expected function name, found {:?}", other),
            )]);
        }
    };
    let params = expr::parse_params(tokens)?;
    let (body, close) = expr::parse_block(tokens)?;
    Ok(Decl::Function(Box::new(FunctionDecl {
        span: Span::new(start.start, close.end),
        name: Some(name),
        params,
        body,
        is_async: false,
        is_generator: false,
    })))
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

fn optional_label(tokens: &mut ParserTokenStream) -> Option<String> {
    if let TokenKind::Ident(n) = tokens.peek_kind().clone() {
        tokens.bump();
        Some(n)
    } else {
        None
    }
}

/// Consume an optional semicolon (minimal ASI: a missing `;` before `}`, EOF
/// or the start of a new statement is tolerated).
fn consume_semicolon(tokens: &mut ParserTokenStream) {
    tokens.eat_punctuator(Punctuator::Semicolon);
}

/// The statement-parsing handle used by the top-level driver. Delegates to the
/// free functions above and exposes the helpers the driver needs.
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

    pub fn span(&mut self) -> Span {
        self.tokens.span()
    }

    pub fn parse_program_item(&mut self) -> Result<ProgramItem, Vec<Diagnostic>> {
        parse_program_item(self.tokens)
    }

    pub fn recover_to_statement_boundary(&mut self) {
        if self.tokens.is_eof() {
            return;
        }
        loop {
            if self.tokens.is_eof() {
                return;
            }
            let boundary = matches!(
                self.tokens.peek_kind(),
                TokenKind::Punctuator(p)
                    if *p == Punctuator::Semicolon || *p == Punctuator::RBrace
            );
            self.tokens.bump();
            if boundary {
                return;
            }
        }
    }
}

// ForInit is part of the AST surface but not used by milestone-1 statements.
#[allow(dead_code)]
fn _for_init_marker() -> Option<ForInit> {
    None
}
