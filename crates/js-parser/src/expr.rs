//! Expression parsing via precedence climbing (Pratt).
//!
//! The public entry points are free functions over a shared
//! [`ParserTokenStream`], so statement parsing can drive expression parsing
//! without borrow gymnastics.
//!
//! Implemented (milestone 1): numeric / string literals, identifiers, `this`,
//! parenthesized expressions, unary prefix, binary operators (via Pratt),
//! assignment, call (`f(args)`), and member access (`o.p` / `o[e]`).

use crate::token_stream::ParserTokenStream;
use js_diagnostics::Diagnostic;
use js_syntax::ast::expr::{
    AssignTarget, ArrowBody, ArrowExpr, ArrayExprElement, CallArg, CallExpr, Expr,
    FunctionExpr, MemberExpr, MemberProp, ObjectProp, ObjectPropKind, ObjectPropValue,
};
use js_syntax::ast::lit::Lit;
use js_syntax::ast::op::{AssignOp, BinOp, UnaryOp};
use js_syntax::ast::pat::PropKey;
use js_syntax::ast::stmt::Stmt;
use js_syntax::keyword::Keyword;
use js_syntax::punctuator::Punctuator;
use js_syntax::source::Span;
use js_syntax::token::TokenKind;

/// Operator binding power pairs: `(left, right)` precedence, used by the Pratt
/// loop. Higher binds tighter. Derived from the ECMAScript grammar.
fn binding_power(op: BinOp) -> Option<(u8, u8)> {
    use BinOp::*;
    Some(match op {
        NullishCoal | Or => (1, 2),
        And => (3, 4),
        BitOr => (5, 6),
        BitXor => (7, 8),
        BitAnd => (9, 10),
        Eq | NotEq | StrictEq | StrictNotEq => (11, 12),
        Lt | Gt | Le | Ge | Instanceof => (13, 14),
        In => (13, 14),
        Shl | Shr | Ushr => (15, 16),
        Add | Sub => (17, 18),
        Mul | Div | Mod => (19, 20),
        Exp => (23, 22), // right-associative
    })
}

/// Parse a full expression (a comma sequence if more than one).
pub fn parse_expression(tokens: &mut ParserTokenStream) -> Result<Expr, Vec<Diagnostic>> {
    let first = parse_assignment(tokens)?;
    // Comma sequence.
    if !tokens.eat_punctuator(Punctuator::Comma) {
        return Ok(first);
    }
    let mut exprs = vec![first];
    loop {
        exprs.push(parse_assignment(tokens)?);
        if !tokens.eat_punctuator(Punctuator::Comma) {
            break;
        }
    }
    let span = Span::new(exprs.first().unwrap().span().start, exprs.last().unwrap().span().end);
    Ok(Expr::Sequence { span, exprs })
}

/// Parse an assignment expression (right-associative).
pub fn parse_assignment(tokens: &mut ParserTokenStream) -> Result<Expr, Vec<Diagnostic>> {
    let lhs = parse_conditional(tokens)?;
    let op = match tokens.peek_kind().clone() {
        TokenKind::Punctuator(p) => match p {
            Punctuator::Assign => AssignOp::Assign,
            Punctuator::AddAssign => AssignOp::Add,
            Punctuator::SubAssign => AssignOp::Sub,
            Punctuator::MulAssign => AssignOp::Mul,
            Punctuator::DivAssign => AssignOp::Div,
            Punctuator::ModAssign => AssignOp::Mod,
            Punctuator::ExpAssign => AssignOp::Exp,
            Punctuator::BitAndAssign => AssignOp::BitAnd,
            Punctuator::BitOrAssign => AssignOp::BitOr,
            Punctuator::BitXorAssign => AssignOp::BitXor,
            Punctuator::ShlAssign => AssignOp::Shl,
            Punctuator::ShrAssign => AssignOp::Shr,
            Punctuator::UshrAssign => AssignOp::Ushr,
            _ => return Ok(lhs),
        },
        _ => return Ok(lhs),
    };
    let eq_span = tokens.span();
    tokens.bump();
    let rhs = parse_assignment(tokens)?;
    let target = match &lhs {
        Expr::Ident { span, name } => AssignTarget::Ident {
            span: *span,
            name: name.clone(),
        },
        Expr::Member(m) => AssignTarget::Member(m.clone()),
        _ => {
            return Err(vec![Diagnostic::error(
                eq_span,
                "invalid assignment target",
            )]);
        }
    };
    let span = Span::new(lhs.span().start, rhs.span().end);
    Ok(Expr::Assign {
        span,
        op,
        left: target,
        right: Box::new(rhs),
    })
}

/// Conditional (`c ? a : b`). Skeleton: parses the test then requires `?:`.
fn parse_conditional(tokens: &mut ParserTokenStream) -> Result<Expr, Vec<Diagnostic>> {
    let test = parse_binary(tokens, 0)?;
    if tokens.eat_punctuator(Punctuator::QuestionMark) {
        let cons = parse_assignment(tokens)?;
        let _ = expect_punctuator(tokens, Punctuator::Colon)?;
        let alt = parse_assignment(tokens)?;
        let span = Span::new(test.span().start, alt.span().end);
        return Ok(Expr::Conditional {
            span,
            test: Box::new(test),
            cons: Box::new(cons),
            alt: Box::new(alt),
        });
    }
    Ok(test)
}

/// The Pratt loop for binary operators with minimum binding power `min_bp`.
fn parse_binary(tokens: &mut ParserTokenStream, min_bp: u8) -> Result<Expr, Vec<Diagnostic>> {
    let mut lhs = parse_unary(tokens)?;
    loop {
        let op = match tokens.peek_kind().clone() {
            TokenKind::Punctuator(p) => match BinOp::from_punctuator(p) {
                Some(o) => o,
                None => break,
            },
            TokenKind::Keyword(Keyword::In) => BinOp::In,
            TokenKind::Keyword(Keyword::Instanceof) => BinOp::Instanceof,
            _ => break,
        };
        let (l_bp, r_bp) = binding_power(op).ok_or_else(|| {
            vec![Diagnostic::error(tokens.span(), "operator has no binding power")]
        })?;
        if l_bp < min_bp {
            break;
        }
        tokens.bump();
        let rhs = parse_binary(tokens, r_bp)?;
        let span = Span::new(lhs.span().start, rhs.span().end);
        lhs = if matches!(op, BinOp::And | BinOp::Or | BinOp::NullishCoal) {
            Expr::Logical {
                span,
                op,
                left: Box::new(lhs),
                right: Box::new(rhs),
            }
        } else {
            Expr::Binary {
                span,
                op,
                left: Box::new(lhs),
                right: Box::new(rhs),
            }
        };
    }
    Ok(lhs)
}

/// Unary prefix operators.
fn parse_unary(tokens: &mut ParserTokenStream) -> Result<Expr, Vec<Diagnostic>> {
    let (op, span) = match tokens.peek_kind().clone() {
        TokenKind::Punctuator(Punctuator::Add) => (Some(UnaryOp::Pos), tokens.span()),
        TokenKind::Punctuator(Punctuator::Sub) => (Some(UnaryOp::Neg), tokens.span()),
        TokenKind::Punctuator(Punctuator::Not) => (Some(UnaryOp::Not), tokens.span()),
        TokenKind::Punctuator(Punctuator::BitNot) => (Some(UnaryOp::BitNot), tokens.span()),
        TokenKind::Keyword(Keyword::Typeof) => (Some(UnaryOp::Typeof), tokens.span()),
        TokenKind::Keyword(Keyword::Void) => (Some(UnaryOp::Void), tokens.span()),
        TokenKind::Keyword(Keyword::Delete) => (Some(UnaryOp::Delete), tokens.span()),
        _ => (None, tokens.span()),
    };
    if let Some(op) = op {
        tokens.bump();
        let arg = parse_unary(tokens)?;
        let span = Span::new(span.start, arg.span().end);
        return Ok(Expr::Unary {
            span,
            op,
            arg: Box::new(arg),
        });
    }
    parse_postfix(tokens)
}

/// Postfix: call and member access.
fn parse_postfix(tokens: &mut ParserTokenStream) -> Result<Expr, Vec<Diagnostic>> {
    let mut expr = parse_primary(tokens)?;
    loop {
        if tokens.eat_punctuator(Punctuator::LParen) {
            let mut args = Vec::new();
            if !matches!(tokens.peek_kind(), TokenKind::Punctuator(Punctuator::RParen)) {
                loop {
                    if tokens.eat_punctuator(Punctuator::Spread) {
                        let e = parse_assignment(tokens)?;
                        args.push(CallArg::Spread(e));
                    } else {
                        args.push(CallArg::Expr(parse_assignment(tokens)?));
                    }
                    if !tokens.eat_punctuator(Punctuator::Comma) {
                        break;
                    }
                }
            }
            let close = expect_punctuator(tokens, Punctuator::RParen)?;
            let span = Span::new(expr.span().start, close.end);
            expr = Expr::Call(Box::new(CallExpr {
                span,
                callee: Box::new(expr),
                args,
                optional: false,
            }));
            continue;
        }
        // Member access.
        if tokens.eat_punctuator(Punctuator::Dot) {
            let name = match tokens.peek_kind().clone() {
                TokenKind::Ident(n) => {
                    tokens.bump();
                    n
                }
                TokenKind::Keyword(kw) => {
                    tokens.bump();
                    kw.as_str().to_string()
                }
                _ => {
                    return Err(vec![Diagnostic::error(
                        tokens.span(),
                        "expected property name after '.'",
                    )]);
                }
            };
            let end = tokens.span();
            let span = Span::new(expr.span().start, end.start);
            expr = Expr::Member(Box::new(MemberExpr {
                span,
                object: Box::new(expr),
                property: MemberProp::Ident(name),
            }));
            continue;
        }
        if tokens.eat_punctuator(Punctuator::LBracket) {
            let idx = parse_expression(tokens)?;
            let close = expect_punctuator(tokens, Punctuator::RBracket)?;
            let span = Span::new(expr.span().start, close.end);
            expr = Expr::Member(Box::new(MemberExpr {
                span,
                object: Box::new(expr),
                property: MemberProp::Computed(Box::new(idx)),
            }));
            continue;
        }
        break;
    }
    Ok(expr)
}

/// Primary expressions.
fn parse_primary(tokens: &mut ParserTokenStream) -> Result<Expr, Vec<Diagnostic>> {
    let token = tokens.bump();
    let span = token.span;
    match token.kind {
        TokenKind::Numeric(raw) => {
            let n: f64 = raw
                .parse()
                .map_err(|_| vec![Diagnostic::error(span, "invalid numeric literal")])?;
            Ok(Expr::Lit(Lit::Number(span, n)))
        }
        TokenKind::Bigint(raw) => Ok(Expr::Lit(Lit::BigInt(span, raw))),
        TokenKind::String(s) => Ok(Expr::Lit(Lit::String(span, s))),
        TokenKind::Ident(name) => Ok(Expr::Ident { span, name }),
        TokenKind::Keyword(Keyword::This) => Ok(Expr::This(span)),
        TokenKind::Keyword(Keyword::True) => Ok(Expr::Lit(Lit::Boolean(span, true))),
        TokenKind::Keyword(Keyword::False) => Ok(Expr::Lit(Lit::Boolean(span, false))),
        TokenKind::Keyword(Keyword::Null) => Ok(Expr::Lit(Lit::Null(span))),
        TokenKind::Keyword(Keyword::Undefined) => Ok(Expr::Ident {
            span,
            name: "undefined".to_string(),
        }),
        TokenKind::Punctuator(Punctuator::LParen) => {
            let inner = parse_expression(tokens)?;
            let close = expect_punctuator(tokens, Punctuator::RParen)?;
            let span = Span::new(span.start, close.end);
            Ok(Expr::Paren {
                span,
                expr: Box::new(inner),
            })
        }
        // Function / arrow expressions.
        TokenKind::Keyword(Keyword::Function) => parse_function_expr(tokens, span),
        other => Err(vec![Diagnostic::error(
            span,
            format!("unexpected token in expression: {:?}", other),
        )]),
    }
}

/// `function name?(params){body}` expression.
fn parse_function_expr(
    tokens: &mut ParserTokenStream,
    start: Span,
) -> Result<Expr, Vec<Diagnostic>> {
    let name = if let TokenKind::Ident(n) = tokens.peek_kind().clone() {
        tokens.bump();
        Some(n)
    } else {
        None
    };
    let params = parse_params(tokens)?;
    let (body, close) = parse_block(tokens)?;
    let span = Span::new(start.start, close.end);
    Ok(Expr::Function(Box::new(FunctionExpr {
        span,
        name,
        params,
        body,
        is_async: false,
        is_generator: false,
    })))
}

/// `(a, b, ...rest)` — parameter list.
pub(crate) fn parse_params(tokens: &mut ParserTokenStream) -> Result<Vec<js_syntax::ast::pat::Pat>, Vec<Diagnostic>> {
    let _ = expect_punctuator(tokens, Punctuator::LParen)?;
    let mut params = Vec::new();
    if !matches!(tokens.peek_kind(), TokenKind::Punctuator(Punctuator::RParen)) {
        loop {
            if tokens.eat_punctuator(Punctuator::Spread) {
                let p = parse_binding_identifier(tokens)?;
                params.push(js_syntax::ast::pat::Pat::Rest {
                    span: p.span(),
                    arg: Box::new(p),
                });
            } else {
                params.push(parse_binding_identifier(tokens)?);
            }
            if !tokens.eat_punctuator(Punctuator::Comma) {
                break;
            }
        }
    }
    let _ = expect_punctuator(tokens, Punctuator::RParen)?;
    Ok(params)
}

/// A simple identifier binding pattern.
fn parse_binding_identifier(
    tokens: &mut ParserTokenStream,
) -> Result<js_syntax::ast::pat::Pat, Vec<Diagnostic>> {
    let span = tokens.span();
    match tokens.peek_kind().clone() {
        TokenKind::Ident(name) => {
            tokens.bump();
            Ok(js_syntax::ast::pat::Pat::Ident { span, name })
        }
        other => Err(vec![Diagnostic::error(
            span,
            format!("expected identifier, found {:?}", other),
        )]),
    }
}

/// Parse a `{ ... }` block, returning the body and the closing-brace span.
pub(crate) fn parse_block(
    tokens: &mut ParserTokenStream,
) -> Result<(Vec<Stmt>, Span), Vec<Diagnostic>> {
    let open = expect_punctuator(tokens, Punctuator::LBrace)?;
    let mut body = Vec::new();
    let mut errors = Vec::new();
    while !matches!(
        tokens.peek_kind(),
        TokenKind::Punctuator(Punctuator::RBrace) | TokenKind::Eof
    ) {
        match crate::stmt::parse_statement(tokens) {
            Ok(s) => body.push(s),
            Err(diags) => {
                errors.extend(diags);
                recover_to_statement_boundary(tokens);
            }
        }
    }
    let close = expect_punctuator(tokens, Punctuator::RBrace)?;
    let _ = open;
    if !errors.is_empty() {
        return Err(errors);
    }
    Ok((body, close))
}

/// Expect a specific punctuator.
pub(crate) fn expect_punctuator(
    tokens: &mut ParserTokenStream,
    p: Punctuator,
) -> Result<Span, Vec<Diagnostic>> {
    if tokens.eat_punctuator(p) {
        Ok(tokens.span())
    } else {
        let span = tokens.span();
        Err(vec![Diagnostic::error(
            span,
            format!("expected `{}`", p.as_str()),
        )])
    }
}

/// Error recovery shared with statement parsing.
fn recover_to_statement_boundary(tokens: &mut ParserTokenStream) {
    if matches!(tokens.peek_kind(), TokenKind::Eof) {
        return;
    }
    loop {
        if matches!(tokens.peek_kind(), TokenKind::Eof) {
            return;
        }
        let boundary = matches!(
            tokens.peek_kind(),
            TokenKind::Punctuator(p)
                if *p == Punctuator::Semicolon || *p == Punctuator::RBrace
        );
        tokens.bump();
        if boundary {
            return;
        }
    }
}

// Keep a few placeholder symbols referenced by the AST re-exports so future
// milestones can extend incrementally without churn here.
#[allow(dead_code)]
fn _unused_ast_types() -> (
    ArrowBody,
    ArrowExpr,
    ArrayExprElement,
    ObjectProp,
    ObjectPropKind,
    ObjectPropValue,
    PropKey,
) {
    unreachable!()
}
