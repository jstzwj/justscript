//! Expression parsing via precedence climbing (Pratt).
//!
//! The public entry points are free functions over a shared
//! [`ParserTokenStream`], so statement parsing can drive expression parsing
//! without borrow gymnastics.

use crate::token_stream::ParserTokenStream;
use js_diagnostics::Diagnostic;
use js_syntax::ast::expr::{
    ArrayExprElement, ArrowBody, ArrowExpr, AssignTarget, CallArg, CallExpr, Expr, FunctionExpr,
    ImportPhase, MemberExpr, MemberProp, NewExpr, ObjectProp, ObjectPropKind, ObjectPropValue,
};
use js_syntax::ast::lit::Lit;
use js_syntax::ast::op::{AssignOp, BinOp, UnaryOp, UpdateOp};
use js_syntax::ast::pat::{ArrayPatElement, ObjectPatProp, Pat, PropKey};
use js_syntax::ast::stmt::Stmt;
use js_syntax::keyword::Keyword;
use js_syntax::punctuator::Punctuator;
use js_syntax::source::Span;
use js_syntax::token::TokenKind;
use std::str::FromStr;

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
    parse_expression_inner(tokens, true)
}

/// Like [`parse_expression`], but when `in_ok` is false the top-level `in`
/// operator is left unconsumed — the `[~In]` grammar used in `for...in` /
/// `for...of` heads. (Only the *immediate* head level is affected; nested
/// bracketed/braced/parenthesized sub-expressions always re-enable `in`.)
pub(crate) fn parse_expression_inner(
    tokens: &mut ParserTokenStream,
    in_ok: bool,
) -> Result<Expr, Vec<Diagnostic>> {
    let first = parse_assignment_inner(tokens, in_ok)?;
    if !tokens.eat_punctuator(Punctuator::Comma) {
        return Ok(first);
    }
    let mut exprs = vec![first];
    loop {
        // After the first comma we're in a sequence — `in` is always allowed.
        exprs.push(parse_assignment(tokens)?);
        if !tokens.eat_punctuator(Punctuator::Comma) {
            break;
        }
    }
    let span = Span::new(
        exprs.first().unwrap().span().start,
        exprs.last().unwrap().span().end,
    );
    Ok(Expr::Sequence { span, exprs })
}

/// Parse an assignment expression (right-associative). Also the entry point for
/// arrow functions (`x => ...`, `(a, b) => ...`), which are detected before the
/// normal assignment path via speculative backtracking.
pub fn parse_assignment(tokens: &mut ParserTokenStream) -> Result<Expr, Vec<Diagnostic>> {
    parse_assignment_inner(tokens, true)
}

fn parse_assignment_inner(
    tokens: &mut ParserTokenStream,
    in_ok: bool,
) -> Result<Expr, Vec<Diagnostic>> {
    // `yield` — a yield expression only inside a generator; outside one,
    // `yield` is a plain identifier reference (sloppy mode).
    if tokens.current_ctx().is_generator
        && matches!(tokens.peek_kind(), TokenKind::Keyword(Keyword::Yield))
    {
        return parse_yield(tokens, in_ok);
    }
    if let Some(arrow) = try_parse_arrow(tokens)? {
        return Ok(arrow);
    }
    let lhs = parse_conditional_inner(tokens, in_ok)?;
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
            Punctuator::AndAssign => AssignOp::And,
            Punctuator::OrAssign => AssignOp::Or,
            Punctuator::NullishAssign => AssignOp::Nullish,
            _ => return Ok(lhs),
        },
        _ => return Ok(lhs),
    };
    let eq_span = tokens.span();
    tokens.bump();
    // The RHS inherits the `In` grammar parameter (`for (x = a in b; …)` blocks `in`).
    let rhs = parse_assignment_inner(tokens, in_ok)?;
    let target_pattern = assignment_pattern_from_expr(&lhs)
        .map_err(|_| vec![Diagnostic::error(eq_span, "invalid assignment target")])?;
    let target = match target_pattern {
        Pat::Ident { span, name } => AssignTarget::Ident { span, name },
        Pat::Member(member) => AssignTarget::Member(member),
        pattern @ (Pat::Array { .. } | Pat::Object { .. }) if op == AssignOp::Assign => {
            AssignTarget::Pat(pattern)
        }
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

/// Speculatively parse an arrow function; on any mismatch restore the stream and
/// return `Ok(None)` so the caller falls back to a normal assignment/paren expr.
fn try_parse_arrow(tokens: &mut ParserTokenStream) -> Result<Option<Expr>, Vec<Diagnostic>> {
    // `Identifier =>` (also a contextual keyword like `yield`/`await` as a name).
    if let Some(name) = peek_binding_name(tokens) {
        if matches!(
            tokens.peek2().kind,
            TokenKind::Punctuator(Punctuator::Arrow)
        ) && !tokens.preceded_by_newline_at(1)
        {
            let snap = tokens.snapshot();
            let start = tokens.span();
            tokens.bump(); // the param name
            tokens.bump(); // `=>`

            // A non-async arrow establishes a return target but inherits the
            // enclosing await/yield grammar parameters lexically.
            tokens.push_ctx(tokens.current_ctx());
            let body = parse_arrow_body(tokens, start);
            tokens.pop_ctx();
            return match body {
                Ok(body) => Ok(Some(Expr::Arrow(Box::new(ArrowExpr {
                    span: arrow_span(start, &body),
                    params: vec![Pat::Ident { span: start, name }],
                    body,
                    is_async: false,
                })))),
                Err(e) => {
                    tokens.restore(snap);
                    Err(e)
                }
            };
        }
    }
    // `( ... ) =>` — only worth trying when we open a paren.
    if matches!(
        tokens.peek_kind(),
        TokenKind::Punctuator(Punctuator::LParen)
    ) {
        let snap = tokens.snapshot();
        match parse_arrow_params(tokens) {
            Ok(params) => {
                if matches!(tokens.peek_kind(), TokenKind::Punctuator(Punctuator::Arrow))
                    && !tokens.preceded_by_newline()
                {
                    let start = tokens.span();
                    let lparen_start = start.start; // overwritten below via snap
                    let _ = lparen_start;
                    tokens.bump(); // `=>`
                    let real_start = first_span_after(snap, tokens);
                    tokens.push_ctx(tokens.current_ctx());
                    let body = parse_arrow_body(tokens, real_start);
                    tokens.pop_ctx();
                    match body {
                        Ok(body) => {
                            return Ok(Some(Expr::Arrow(Box::new(ArrowExpr {
                                span: arrow_span(real_start, &body),
                                params,
                                body,
                                is_async: false,
                            }))));
                        }
                        Err(e) => {
                            tokens.restore(snap);
                            return Err(e);
                        }
                    }
                }
            }
            Err(_) => {}
        }
        tokens.restore(snap);
    }
    Ok(None)
}

/// `async`-prefixed arrow (`async x => ...`, `async (a,b) => ...`), or fall
/// back to treating `async` as a plain identifier. The `async` keyword token is
/// already consumed by the caller.
fn try_parse_async_arrow(
    tokens: &mut ParserTokenStream,
    start: Span,
) -> Result<Expr, Vec<Diagnostic>> {
    // An escaped `async` is an IdentifierReference, never the grammar terminal
    // introducing an async arrow. A line terminator after `async` likewise
    // prevents the async-arrow production and leaves ordinary expression/ASI
    // parsing to determine whether the surrounding source is valid.
    if tokens.token_span_contains_escape(start) || tokens.preceded_by_newline() {
        return Ok(Expr::Ident {
            span: start,
            name: "async".to_string(),
        });
    }
    let snap = tokens.snapshot();
    // `async <binding> =>` — a single-parameter async arrow. The parameter may
    // be a contextual keyword (`async of => …`, `async as => …`).
    if matches!(
        tokens.peek2().kind,
        TokenKind::Punctuator(Punctuator::Arrow)
    ) && !tokens.preceded_by_newline_at(1)
        && peek_binding_name(tokens).is_some()
    {
        let name = binding_identifier(tokens).unwrap_or_default();
        tokens.bump(); // `=>`
                       // Async-arrow body is an async context (`await` reserved).
        tokens.enter_fn(true, false);
        let body = parse_arrow_body(tokens, start);
        tokens.pop_ctx();
        return match body {
            Ok(body) => Ok(Expr::Arrow(Box::new(ArrowExpr {
                span: arrow_span(start, &body),
                params: vec![Pat::Ident { span: start, name }],
                body,
                is_async: true,
            }))),
            Err(e) => {
                tokens.restore(snap);
                Err(e)
            }
        };
    }
    // `async ( params ) =>`
    if matches!(
        tokens.peek_kind(),
        TokenKind::Punctuator(Punctuator::LParen)
    ) {
        tokens.enter_fn(true, false);
        let parsed = parse_arrow_params(tokens);
        let is_arrow = parsed.is_ok()
            && matches!(tokens.peek_kind(), TokenKind::Punctuator(Punctuator::Arrow))
            && !tokens.preceded_by_newline();
        if is_arrow {
            let params = parsed.unwrap();
            tokens.bump(); // `=>`
            let body = parse_arrow_body(tokens, start);
            tokens.pop_ctx();
            return match body {
                Ok(body) => Ok(Expr::Arrow(Box::new(ArrowExpr {
                    span: arrow_span(start, &body),
                    params,
                    body,
                    is_async: true,
                }))),
                Err(e) => {
                    tokens.restore(snap);
                    Err(e)
                }
            };
        }
        tokens.pop_ctx();
        tokens.restore(snap);
    }
    // Not an async arrow — `async` is a plain identifier reference.
    Ok(Expr::Ident {
        span: start,
        name: "async".to_string(),
    })
}

/// Best-effort span for the arrow start.
fn first_span_after(_snap: usize, _tokens: &ParserTokenStream) -> Span {
    Span::DUMMY
}

fn arrow_span(start: Span, body: &ArrowBody) -> Span {
    let end = match body {
        ArrowBody::Block(_) => start.end,
        ArrowBody::Expr(_) => start.end,
    };
    Span::new(start.start, end)
}

/// Parse the body of an arrow function: either a `{ ... }` block or a concise
/// assignment expression.
fn parse_arrow_body(
    tokens: &mut ParserTokenStream,
    _start: Span,
) -> Result<ArrowBody, Vec<Diagnostic>> {
    if matches!(
        tokens.peek_kind(),
        TokenKind::Punctuator(Punctuator::LBrace)
    ) {
        let (body, _) = parse_block(tokens)?;
        Ok(ArrowBody::Block(body))
    } else {
        let e = parse_assignment(tokens)?;
        Ok(ArrowBody::Expr(Box::new(e)))
    }
}

/// Parse `(` <binding-pattern-list> `)` for an arrow head. Returns `Err` (not a
/// diagnostic) if the input is not a valid parameter list — the caller restores.
fn parse_arrow_params(tokens: &mut ParserTokenStream) -> Result<Vec<Pat>, ()> {
    if !matches!(
        tokens.peek_kind(),
        TokenKind::Punctuator(Punctuator::LParen)
    ) {
        return Err(());
    }
    tokens.bump();
    let mut params = Vec::new();
    if matches!(
        tokens.peek_kind(),
        TokenKind::Punctuator(Punctuator::RParen)
    ) {
        tokens.bump();
        return Ok(params);
    }
    loop {
        // Rest parameter `...pat` — must be the last parameter.
        if tokens.eat_punctuator(Punctuator::Spread) {
            match parse_binding_pattern(tokens) {
                Ok(p) => {
                    let span = p.span();
                    params.push(Pat::Rest {
                        span,
                        arg: Box::new(p),
                    });
                }
                Err(_) => return Err(()),
            }
            // A rest parameter must be immediately followed by `)`.
            if !matches!(
                tokens.peek_kind(),
                TokenKind::Punctuator(Punctuator::RParen)
            ) {
                return Err(());
            }
            tokens.bump();
            return Ok(params);
        }
        match parse_binding_pattern(tokens) {
            Ok(p) => params.push(p),
            Err(_) => return Err(()),
        }
        match tokens.peek_kind() {
            TokenKind::Punctuator(Punctuator::Comma) => {
                tokens.bump();
                // Trailing comma before `)`.
                if matches!(
                    tokens.peek_kind(),
                    TokenKind::Punctuator(Punctuator::RParen)
                ) {
                    tokens.bump();
                    return Ok(params);
                }
            }
            TokenKind::Punctuator(Punctuator::RParen) => {
                tokens.bump();
                return Ok(params);
            }
            _ => return Err(()),
        }
    }
}

/// `yield`, `yield expr`, or `yield* expr` (delegate). Restricted production:
/// a line terminator before the operand means no operand.
fn parse_yield(tokens: &mut ParserTokenStream, in_ok: bool) -> Result<Expr, Vec<Diagnostic>> {
    let start = tokens.span();
    tokens.bump(); // `yield`
    let delegate = !tokens.preceded_by_newline() && tokens.eat_punctuator(Punctuator::Mul);
    // No operand if a newline (or `;`/`}`/`)`/EOF) follows.
    let stop = matches!(
        tokens.peek_kind(),
        TokenKind::Punctuator(Punctuator::Semicolon)
            | TokenKind::Punctuator(Punctuator::RBrace)
            | TokenKind::Punctuator(Punctuator::RParen)
            | TokenKind::Punctuator(Punctuator::Comma)
            | TokenKind::Punctuator(Punctuator::Colon)
            | TokenKind::Punctuator(Punctuator::RBracket)
            | TokenKind::Eof
    ) || (!delegate && tokens.preceded_by_newline());
    let arg = if stop {
        None
    } else {
        Some(Box::new(parse_assignment_inner(tokens, in_ok)?))
    };
    let end = arg.as_ref().map(|e| e.span().end).unwrap_or(start.end);
    Ok(Expr::Yield {
        span: Span::new(start.start, end),
        arg,
        delegate,
    })
}

/// Conditional (`c ? a : b`).
fn parse_conditional_inner(
    tokens: &mut ParserTokenStream,
    in_ok: bool,
) -> Result<Expr, Vec<Diagnostic>> {
    let test = parse_binary_inner(tokens, 0, in_ok)?;
    if tokens.eat_punctuator(Punctuator::QuestionMark) {
        // ConditionalExpression[In] always parses its consequent with [+In];
        // only the test and alternate inherit the surrounding grammar flag.
        let cons = parse_assignment_inner(tokens, true)?;
        let _ = expect_punctuator(tokens, Punctuator::Colon)?;
        let alt = parse_assignment_inner(tokens, in_ok)?;
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
fn parse_binary_inner(
    tokens: &mut ParserTokenStream,
    min_bp: u8,
    in_ok: bool,
) -> Result<Expr, Vec<Diagnostic>> {
    let mut lhs = if matches!(tokens.peek_kind(), TokenKind::PrivateName(_)) {
        parse_private_in(tokens, min_bp, in_ok)?
    } else {
        parse_unary(tokens)?
    };
    loop {
        // Operator position: a `/` here is always the division operator, never a
        // regex (a regex literal never serves as a binary operator). The lexer's
        // previous-token heuristic can mis-classify it as a regex — most often
        // after a `}` that closed a value-producing expression such as an object
        // literal (`{a: 1} / 2`), a function/class expression, or an arrow block
        // — so re-lex under the division goal before matching the operator.
        match tokens.peek_kind() {
            TokenKind::Regex { .. } | TokenKind::Unknown('/') => {
                tokens.reslash_div();
            }
            _ => {}
        }
        let op = match tokens.peek_kind().clone() {
            TokenKind::Punctuator(p) => match BinOp::from_punctuator(p) {
                Some(o) => o,
                None => break,
            },
            TokenKind::Keyword(Keyword::In) if !in_ok => break,
            TokenKind::Keyword(Keyword::In) => BinOp::In,
            TokenKind::Keyword(Keyword::Instanceof) => BinOp::Instanceof,
            _ => break,
        };
        let (l_bp, r_bp) = binding_power(op).ok_or_else(|| {
            vec![Diagnostic::error(
                tokens.span(),
                "operator has no binding power",
            )]
        })?;
        if l_bp < min_bp {
            break;
        }
        if forbidden_coalesce_mix(op, &lhs) {
            return Err(vec![Diagnostic::error(
                tokens.span(),
                "nullish coalescing may not be mixed with `&&` or `||` without parentheses",
            )]);
        }
        if op == BinOp::Exp && matches!(lhs, Expr::Unary { .. } | Expr::Await { .. }) {
            return Err(vec![Diagnostic::error(
                tokens.span(),
                "an unparenthesized unary expression may not be the left operand of `**`",
            )]);
        }
        tokens.bump();
        let rhs = parse_binary_inner(tokens, r_bp, in_ok)?;
        if forbidden_coalesce_mix(op, &rhs) {
            return Err(vec![Diagnostic::error(
                rhs.span(),
                "nullish coalescing may not be mixed with `&&` or `||` without parentheses",
            )]);
        }
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

/// Parse the dedicated `PrivateIdentifier in ShiftExpression` relational
/// production. A private identifier is not a PrimaryExpression, so this entry
/// is available only at relational precedence and only when `[In]` is enabled.
fn parse_private_in(
    tokens: &mut ParserTokenStream,
    min_bp: u8,
    in_ok: bool,
) -> Result<Expr, Vec<Diagnostic>> {
    let (relational_bp, shift_expression_bp) = binding_power(BinOp::In).unwrap();
    let private = tokens.bump();
    let name = match private.kind {
        TokenKind::PrivateName(name) => name,
        _ => unreachable!(),
    };
    if min_bp > relational_bp || !in_ok || !tokens.is_unescaped_keyword_at(0, Keyword::In) {
        return Err(vec![Diagnostic::error(
            private.span,
            "a private identifier is only valid as the left operand of `in`",
        )]);
    }
    tokens.bump(); // `in`
    let right = parse_binary_inner(tokens, shift_expression_bp, true)?;
    Ok(Expr::PrivateIn {
        span: Span::new(private.span.start, right.span().end),
        name,
        right: Box::new(right),
    })
}

/// Unary prefix operators, including prefix `++` / `--`.
fn parse_unary(tokens: &mut ParserTokenStream) -> Result<Expr, Vec<Diagnostic>> {
    // `await expr` — a prefix unary operator only inside an async function;
    // outside one, `await` is a plain identifier reference (sloppy mode).
    if tokens.current_ctx().is_async
        && matches!(tokens.peek_kind(), TokenKind::Keyword(Keyword::Await))
    {
        let span = tokens.span();
        if tokens.current_token_contains_escape() {
            return Err(vec![Diagnostic::error(
                span,
                "the `await` terminal may not contain an escape sequence",
            )]);
        }
        tokens.bump();
        let arg = parse_unary(tokens)?;
        let span = Span::new(span.start, arg.span().end);
        return Ok(Expr::Await {
            span,
            arg: Box::new(arg),
        });
    }
    let (op, span) = match tokens.peek_kind().clone() {
        TokenKind::Punctuator(Punctuator::Add) => (Some(UnaryOp::Pos), tokens.span()),
        TokenKind::Punctuator(Punctuator::Sub) => (Some(UnaryOp::Neg), tokens.span()),
        TokenKind::Punctuator(Punctuator::Not) => (Some(UnaryOp::Not), tokens.span()),
        TokenKind::Punctuator(Punctuator::BitNot) => (Some(UnaryOp::BitNot), tokens.span()),
        TokenKind::Keyword(Keyword::Typeof) => (Some(UnaryOp::Typeof), tokens.span()),
        TokenKind::Keyword(Keyword::Void) => (Some(UnaryOp::Void), tokens.span()),
        TokenKind::Keyword(Keyword::Delete) => (Some(UnaryOp::Delete), tokens.span()),
        // Prefix update.
        TokenKind::Punctuator(Punctuator::Inc) => {
            let span = tokens.span();
            tokens.bump();
            let arg = parse_unary(tokens)?;
            let span = Span::new(span.start, arg.span().end);
            return Ok(Expr::Update {
                span,
                op: UpdateOp::Inc,
                prefix: true,
                arg: Box::new(arg),
            });
        }
        TokenKind::Punctuator(Punctuator::Dec) => {
            let span = tokens.span();
            tokens.bump();
            let arg = parse_unary(tokens)?;
            let span = Span::new(span.start, arg.span().end);
            return Ok(Expr::Update {
                span,
                op: UpdateOp::Dec,
                prefix: true,
                arg: Box::new(arg),
            });
        }
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
    parse_lhs(tokens)
}

/// Left-hand-side expressions: `new`, member access, calls, postfix `++`/`--`.
pub(crate) fn parse_lhs(tokens: &mut ParserTokenStream) -> Result<Expr, Vec<Diagnostic>> {
    // Operand start: a leading `/` is always a regex literal here, never the
    // division operator (a division operator never begins an operand). The
    // lexer's previous-token heuristic can mis-classify it as division — most
    // often after a statement-header `)` such as `if (x) /re/.test(y)` — so
    // re-lex under the regex goal before parsing the operand.
    match tokens.peek_kind() {
        TokenKind::Punctuator(Punctuator::Div) | TokenKind::Punctuator(Punctuator::DivAssign) => {
            tokens.reslash_regex();
        }
        _ => {}
    }
    let mut expr = if matches!(tokens.peek_kind(), TokenKind::Keyword(Keyword::New)) {
        parse_new_expr(tokens)?
    } else {
        parse_primary(tokens)?
    };
    parse_postfix_chain(tokens, &mut expr)?;
    Ok(expr)
}

/// After a primary or `new`, consume any chain of `(args)`, `.prop`, `[expr]`
/// and a trailing postfix `++`/`--` (restricted production: no newline before).
fn parse_postfix_chain(
    tokens: &mut ParserTokenStream,
    expr: &mut Expr,
) -> Result<(), Vec<Diagnostic>> {
    loop {
        match tokens.peek_kind() {
            TokenKind::Punctuator(Punctuator::LParen) => {
                let args = parse_call_args(tokens)?;
                let span = Span::new(
                    expr.span().start,
                    last_arg_end(&args).unwrap_or(expr.span().end),
                );
                *expr = Expr::Call(Box::new(CallExpr {
                    span,
                    callee: Box::new(std::mem::replace(expr, Expr::This(Span::DUMMY))),
                    args,
                    optional: false,
                }));
            }
            // Optional chaining `?.` — splits into `?.name` / `?.#priv`,
            // `?.[expr]`, and `?.(args)`.
            TokenKind::Punctuator(Punctuator::OptChain) => {
                tokens.bump(); // `?.`
                match tokens.peek_kind() {
                    TokenKind::Punctuator(Punctuator::LParen) => {
                        let args = parse_call_args(tokens)?;
                        let span = Span::new(
                            expr.span().start,
                            last_arg_end(&args).unwrap_or(expr.span().end),
                        );
                        *expr = Expr::Call(Box::new(CallExpr {
                            span,
                            callee: Box::new(std::mem::replace(expr, Expr::This(Span::DUMMY))),
                            args,
                            optional: true,
                        }));
                    }
                    TokenKind::Punctuator(Punctuator::LBracket) => {
                        tokens.bump();
                        let idx = parse_expression(tokens)?;
                        let close = expect_punctuator(tokens, Punctuator::RBracket)?;
                        let span = Span::new(expr.span().start, close.end);
                        *expr = Expr::Member(Box::new(MemberExpr {
                            span,
                            object: Box::new(std::mem::replace(expr, Expr::This(Span::DUMMY))),
                            property: MemberProp::Computed(Box::new(idx)),
                            optional: true,
                        }));
                    }
                    _ => {
                        let (name, is_private) = parse_property_name(tokens)?;
                        let end = tokens.span();
                        let span = Span::new(expr.span().start, end.start);
                        let prop = if is_private {
                            MemberProp::Private(name)
                        } else {
                            MemberProp::Ident(name)
                        };
                        *expr = Expr::Member(Box::new(MemberExpr {
                            span,
                            object: Box::new(std::mem::replace(expr, Expr::This(Span::DUMMY))),
                            property: prop,
                            optional: true,
                        }));
                    }
                }
            }
            TokenKind::Punctuator(Punctuator::Dot) => {
                tokens.bump();
                let (name, is_private) = parse_property_name(tokens)?;
                let end = tokens.span();
                let span = Span::new(expr.span().start, end.start);
                let prop = if is_private {
                    MemberProp::Private(name)
                } else {
                    MemberProp::Ident(name)
                };
                *expr = Expr::Member(Box::new(MemberExpr {
                    span,
                    object: Box::new(std::mem::replace(expr, Expr::This(Span::DUMMY))),
                    property: prop,
                    optional: false,
                }));
            }
            TokenKind::Punctuator(Punctuator::LBracket) => {
                tokens.bump();
                let idx = parse_expression(tokens)?;
                let close = expect_punctuator(tokens, Punctuator::RBracket)?;
                let span = Span::new(expr.span().start, close.end);
                *expr = Expr::Member(Box::new(MemberExpr {
                    span,
                    object: Box::new(std::mem::replace(expr, Expr::This(Span::DUMMY))),
                    property: MemberProp::Computed(Box::new(idx)),
                    optional: false,
                }));
            }
            // Tagged template: `tag\`…\`` / `obj.method\`…\``. The current token
            // is the template's first chunk.
            TokenKind::Template { .. } => {
                if has_unparenthesized_optional_chain(expr) {
                    return Err(vec![Diagnostic::error(
                        expr.span(),
                        "an optional chain may not be used directly as a tagged-template tag",
                    )]);
                }
                let (raw, cooked, tail) = match tokens.peek_kind() {
                    TokenKind::Template { raw, cooked, tail } => {
                        (raw.clone(), cooked.clone(), *tail)
                    }
                    _ => unreachable!(),
                };
                let tok = tokens.bump();
                let template = parse_template(tokens, tok.span, raw, cooked, tail, true)?;
                let span = Span::new(expr.span().start, template.span().end);
                *expr = Expr::TaggedTemplate {
                    span,
                    tag: Box::new(std::mem::replace(expr, Expr::This(Span::DUMMY))),
                    template: Box::new(template),
                };
            }
            // Postfix update — restricted production: a line terminator before
            // `++`/`--` forbids it (`x\n++` is not postfix).
            TokenKind::Punctuator(Punctuator::Inc) if !tokens.preceded_by_newline() => {
                let start = expr.span().start;
                let end = tokens.span().end;
                tokens.bump();
                *expr = Expr::Update {
                    span: Span::new(start, end),
                    op: UpdateOp::Inc,
                    prefix: false,
                    arg: Box::new(std::mem::replace(expr, Expr::This(Span::DUMMY))),
                };
                return Ok(());
            }
            TokenKind::Punctuator(Punctuator::Dec) if !tokens.preceded_by_newline() => {
                let start = expr.span().start;
                let end = tokens.span().end;
                tokens.bump();
                *expr = Expr::Update {
                    span: Span::new(start, end),
                    op: UpdateOp::Dec,
                    prefix: false,
                    arg: Box::new(std::mem::replace(expr, Expr::This(Span::DUMMY))),
                };
                return Ok(());
            }
            _ => break,
        }
    }
    Ok(())
}

fn last_arg_end(args: &[CallArg]) -> Option<js_syntax::source::BytePos> {
    args.last().map(|a| match a {
        CallArg::Expr(e) | CallArg::Spread(e) => e.span().end,
    })
}

fn forbidden_coalesce_mix(operator: BinOp, expression: &Expr) -> bool {
    match expression {
        Expr::Logical { op, .. } => match operator {
            BinOp::NullishCoal => matches!(op, BinOp::And | BinOp::Or),
            BinOp::And | BinOp::Or => *op == BinOp::NullishCoal,
            _ => false,
        },
        _ => false,
    }
}

pub(crate) fn has_unparenthesized_optional_chain(expression: &Expr) -> bool {
    match expression {
        Expr::Member(member) => {
            member.optional || has_unparenthesized_optional_chain(&member.object)
        }
        Expr::Call(call) => call.optional || has_unparenthesized_optional_chain(&call.callee),
        // Parentheses terminate an OptionalChain grammar production.
        _ => false,
    }
}

/// `new` expression: `new C`, `new C(args)`, `new C.x.y(args)`, `new.target`.
fn parse_new_expr(tokens: &mut ParserTokenStream) -> Result<Expr, Vec<Diagnostic>> {
    let start = tokens.span();
    if tokens.current_token_contains_escape() {
        return Err(vec![Diagnostic::error(
            start,
            "the `new` terminal may not contain an escape sequence",
        )]);
    }
    tokens.bump(); // `new`
                   // `new.target`
    if tokens.eat_punctuator(Punctuator::Dot) {
        if matches!(tokens.peek_kind(), TokenKind::Ident(name) if name == "target")
            && !tokens.current_token_contains_escape()
        {
            let target = tokens.bump();
            return Ok(Expr::NewTarget(Span::new(start.start, target.span.end)));
        }
        return Err(vec![Diagnostic::error(
            tokens.span(),
            "expected the unescaped identifier `target` after `new.`",
        )]);
    }
    // Callee: either another `new` (right-assoc) or a primary followed by a
    // member-only chain (no calls — the first `(...)` belongs to `new`).
    let mut callee = if matches!(tokens.peek_kind(), TokenKind::Keyword(Keyword::New)) {
        parse_new_expr(tokens)?
    } else {
        let mut e = parse_primary(tokens)?;
        loop {
            match tokens.peek_kind() {
                TokenKind::Punctuator(Punctuator::Dot) => {
                    tokens.bump();
                    let (name, is_private) = parse_property_name(tokens)?;
                    let end = tokens.span();
                    let prop = if is_private {
                        MemberProp::Private(name)
                    } else {
                        MemberProp::Ident(name)
                    };
                    e = Expr::Member(Box::new(MemberExpr {
                        span: Span::new(e.span().start, end.start),
                        object: Box::new(e),
                        property: prop,
                        optional: false,
                    }));
                }
                TokenKind::Punctuator(Punctuator::LBracket) => {
                    tokens.bump();
                    let idx = parse_expression(tokens)?;
                    let close = expect_punctuator(tokens, Punctuator::RBracket)?;
                    e = Expr::Member(Box::new(MemberExpr {
                        span: Span::new(e.span().start, close.end),
                        object: Box::new(e),
                        property: MemberProp::Computed(Box::new(idx)),
                        optional: false,
                    }));
                }
                _ => break,
            }
        }
        e
    };
    // Optional constructor argument list.
    if unparenthesized_import_call_root(&callee) {
        return Err(vec![Diagnostic::error(
            callee.span(),
            "an import call cannot be used as an unparenthesized `new` callee",
        )]);
    }
    let mut args_end = callee.span().end;
    if matches!(
        tokens.peek_kind(),
        TokenKind::Punctuator(Punctuator::LParen)
    ) {
        let args = parse_call_args(tokens)?;
        args_end = last_arg_end(&args).unwrap_or(args_end);
        let span = Span::new(start.start, args_end);
        callee = Expr::New(Box::new(NewExpr {
            span,
            callee: Box::new(callee),
            args,
        }));
    } else {
        let span = Span::new(start.start, args_end);
        callee = Expr::New(Box::new(NewExpr {
            span,
            callee: Box::new(callee),
            args: Vec::new(),
        }));
    }
    Ok(callee)
}

/// Member access does not turn an ImportCall (a CallExpression) into the
/// MemberExpression required as the operand of bare `new`. Parentheses do:
/// `new import('x').p` is invalid, while `new (import('x').p)` is syntactically
/// valid and may fail later at runtime if the value is not constructable.
fn unparenthesized_import_call_root(expr: &Expr) -> bool {
    match expr {
        Expr::ImportCall { .. } => true,
        Expr::Member(member) => unparenthesized_import_call_root(&member.object),
        _ => false,
    }
}

/// Parse `( arg, ... )` — a call/constructor argument list (without the leading
/// paren consumed check; the caller ensures it).
pub(crate) fn parse_call_args(
    tokens: &mut ParserTokenStream,
) -> Result<Vec<CallArg>, Vec<Diagnostic>> {
    let _ = expect_punctuator(tokens, Punctuator::LParen)?;
    let mut args = Vec::new();
    if !matches!(
        tokens.peek_kind(),
        TokenKind::Punctuator(Punctuator::RParen)
    ) {
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
            // Trailing comma: `f(a, b,)`.
            if matches!(
                tokens.peek_kind(),
                TokenKind::Punctuator(Punctuator::RParen)
            ) {
                break;
            }
        }
    }
    let _ = expect_punctuator(tokens, Punctuator::RParen)?;
    Ok(args)
}

/// Whether a keyword may appear as a shorthand property name (i.e. may be an
/// ordinary identifier in some mode). Contextual/strict-only words are allowed;
/// hard reserved words (var/continue/…) are not.
fn shorthand_keyword_allowed(kw: Keyword) -> bool {
    matches!(
        kw,
        Keyword::Let
            | Keyword::Static
            | Keyword::Async
            | Keyword::Await
            | Keyword::Yield
            | Keyword::Of
            | Keyword::From
            | Keyword::As
            | Keyword::Get
            | Keyword::Set
            | Keyword::Undefined
    )
}

/// The property name after `.`: an IdentifierName (including keyword spellings)
/// or a PrivateIdentifier. Literal property keys are valid only in brackets or
/// object/class definitions, never directly after `.`.
pub(crate) fn parse_property_name(
    tokens: &mut ParserTokenStream,
) -> Result<(String, bool), Vec<Diagnostic>> {
    match tokens.peek_kind().clone() {
        TokenKind::Ident(n) => {
            tokens.bump();
            Ok((n, false))
        }
        TokenKind::PrivateName(n) => {
            tokens.bump();
            Ok((n, true))
        }
        TokenKind::Keyword(kw) => {
            tokens.bump();
            Ok((kw.as_str().to_string(), false))
        }
        _ => Err(vec![Diagnostic::error(
            tokens.span(),
            "expected property name after '.'",
        )]),
    }
}

/// Primary expressions.
fn parse_primary(tokens: &mut ParserTokenStream) -> Result<Expr, Vec<Diagnostic>> {
    // Decorated class expression `@dec class { … }` — intercepted before the
    // uniform `bump()` below because decorators start with `@`.
    if matches!(tokens.peek_kind(), TokenKind::Punctuator(Punctuator::At)) {
        let span = tokens.span();
        let decorators = crate::class::parse_decorator_list(tokens)?;
        let cls_tok = tokens.bump();
        if !matches!(cls_tok.kind, TokenKind::Keyword(Keyword::Class)) {
            return Err(vec![Diagnostic::error(
                cls_tok.span,
                "a decorator may only precede a class",
            )]);
        }
        return crate::class::parse_class_expr(tokens, span, decorators);
    }

    let token = tokens.bump();
    let span = token.span;
    match token.kind {
        TokenKind::Numeric(raw) => {
            let n = js_lexer::parse_number(&raw)
                .map_err(|e| vec![Diagnostic::error(span, e.message())])?;
            Ok(Expr::Lit(Lit::Number(span, n, raw)))
        }
        TokenKind::Bigint(raw) => {
            js_lexer::validate_numeric_literal(&raw)
                .map_err(|e| vec![Diagnostic::error(span, e.message())])?;
            Ok(Expr::Lit(Lit::BigInt(span, raw)))
        }
        TokenKind::String(s) => Ok(Expr::Lit(Lit::String(
            span,
            s,
            tokens
                .token_span_snippet(span)
                .is_some_and(string_contains_legacy_escape),
        ))),
        TokenKind::Regex { pattern, flags } => {
            crate::regexp::validate_pattern(&pattern, &flags)
                .map_err(|message| vec![Diagnostic::error(span, message)])?;
            Ok(Expr::Regex {
                span,
                pattern,
                flags,
            })
        }
        TokenKind::Template { raw, cooked, tail } => {
            parse_template(tokens, span, raw, cooked, tail, false)
        }
        TokenKind::Ident(name) => Ok(Expr::Ident { span, name }),
        TokenKind::PrivateName(_) => Err(vec![Diagnostic::error(
            span,
            "a private identifier is only valid as the left operand of `in`",
        )]),
        TokenKind::Keyword(Keyword::This) => Ok(Expr::This(span)),
        TokenKind::Keyword(Keyword::Super) => Ok(Expr::Super(span)),
        TokenKind::Keyword(Keyword::New) => {
            // `new` reached primary parsing (e.g. inside a member chain) — defer.
            parse_new_from_primary(tokens, span)
        }
        TokenKind::Keyword(Keyword::True) => {
            reject_escaped_keyword_terminal(tokens, span, "true")?;
            Ok(Expr::Lit(Lit::Boolean(span, true)))
        }
        TokenKind::Keyword(Keyword::False) => {
            reject_escaped_keyword_terminal(tokens, span, "false")?;
            Ok(Expr::Lit(Lit::Boolean(span, false)))
        }
        TokenKind::Keyword(Keyword::Null) => {
            reject_escaped_keyword_terminal(tokens, span, "null")?;
            Ok(Expr::Lit(Lit::Null(span)))
        }
        TokenKind::Keyword(Keyword::Function) => parse_function_expr(tokens, span, false),
        TokenKind::Keyword(Keyword::Async)
            if !tokens.token_span_contains_escape(span)
                && tokens.is_unescaped_keyword_at(0, Keyword::Function)
                && !tokens.preceded_by_newline() =>
        {
            // `async function` expression (`async` was already consumed by the
            // `bump()` above; the next token is `function`).
            tokens.bump(); // `function`
            parse_function_expr(tokens, span, true)
        }
        TokenKind::Keyword(Keyword::Async) => {
            // `async` arrow: `async x => ...` / `async (a, b) => ...`.
            // Fall through to arrow detection, which handles `async`-prefixed
            // params by first consuming `async`.
            try_parse_async_arrow(tokens, span)
        }
        TokenKind::Keyword(Keyword::Class) => {
            crate::class::parse_class_expr(tokens, span, Vec::new())
        }
        TokenKind::Keyword(Keyword::Import) => {
            if tokens.token_span_contains_escape(span) {
                Err(vec![Diagnostic::error(
                    span,
                    "the `import` terminal may not contain an escape sequence",
                )])
            } else {
                parse_import_call_or_meta(tokens, span)
            }
        }
        TokenKind::Keyword(Keyword::Undefined) => Ok(Expr::Ident {
            span,
            name: "undefined".to_string(),
        }),
        // `let` / `static` are contextual keywords — usable as identifier
        // references in *sloppy* mode (`for (let in obj)`, `let = 1;`). Gated
        // on the current strict context (directive-prologue strict mode makes
        // them reserved, so they must not be identifiers there).
        TokenKind::Keyword(Keyword::Let) if !tokens.current_ctx().is_strict => Ok(Expr::Ident {
            span,
            name: "let".to_string(),
        }),
        TokenKind::Keyword(Keyword::Static) if !tokens.current_ctx().is_strict => Ok(Expr::Ident {
            span,
            name: "static".to_string(),
        }),
        // `await`/`yield` are plain identifier references outside async /
        // generator contexts (sloppy mode).
        TokenKind::Keyword(Keyword::Await) if !tokens.current_ctx().is_async => Ok(Expr::Ident {
            span,
            name: "await".to_string(),
        }),
        TokenKind::Keyword(Keyword::Yield) if !tokens.current_ctx().is_generator => {
            Ok(Expr::Ident {
                span,
                name: "yield".to_string(),
            })
        }
        // Pure contextual keywords — always usable as identifier references
        // (`set.add`, `var of`, `obj.from`). They have no reserved-word
        // meaning outside specific syntactic positions.
        TokenKind::Keyword(Keyword::Get) => Ok(Expr::Ident {
            span,
            name: "get".to_string(),
        }),
        TokenKind::Keyword(Keyword::Set) => Ok(Expr::Ident {
            span,
            name: "set".to_string(),
        }),
        TokenKind::Keyword(Keyword::Of) => Ok(Expr::Ident {
            span,
            name: "of".to_string(),
        }),
        TokenKind::Keyword(Keyword::From) => Ok(Expr::Ident {
            span,
            name: "from".to_string(),
        }),
        TokenKind::Keyword(Keyword::As) => Ok(Expr::Ident {
            span,
            name: "as".to_string(),
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
        TokenKind::Punctuator(Punctuator::LBracket) => parse_array_literal(tokens, span),
        TokenKind::Punctuator(Punctuator::LBrace) => parse_object_literal(tokens, span),
        other => Err(vec![Diagnostic::error(
            span,
            format!("unexpected token in expression: {:?}", other),
        )]),
    }
}

fn reject_escaped_keyword_terminal(
    tokens: &ParserTokenStream,
    span: Span,
    spelling: &str,
) -> Result<(), Vec<Diagnostic>> {
    if tokens.token_span_contains_escape(span) {
        Err(vec![Diagnostic::error(
            span,
            format!("the `{spelling}` terminal may not contain an escape sequence"),
        )])
    } else {
        Ok(())
    }
}

/// `import` reached in expression position: `import.meta`, a dynamic import
/// call `import(source)` / `import(source, options)`, or the phase forms
/// `import.source(...)` / `import.defer(...)`.
fn parse_import_call_or_meta(
    tokens: &mut ParserTokenStream,
    start: Span,
) -> Result<Expr, Vec<Diagnostic>> {
    if tokens.eat_punctuator(Punctuator::Dot) {
        // `import.meta` / `import.source(...)` / `import.defer(...)`.
        let prop = tokens.bump();
        if tokens.token_span_contains_escape(prop.span) {
            return Err(vec![Diagnostic::error(
                prop.span,
                "an import phase terminal may not contain an escape sequence",
            )]);
        }
        return match prop.kind {
            TokenKind::Ident(name) if name == "meta" => {
                Ok(Expr::ImportMeta(Span::new(start.start, prop.span.end)))
            }
            TokenKind::Ident(name) if name == "source" => {
                parse_import_call_tail(tokens, start, ImportPhase::Source)
            }
            TokenKind::Ident(name) if name == "defer" => {
                parse_import_call_tail(tokens, start, ImportPhase::Defer)
            }
            other => Err(vec![Diagnostic::error(
                prop.span,
                format!(
                    "expected `meta`, `source`, or `defer` after `import.`, found {:?}",
                    other
                ),
            )]),
        };
    }
    parse_import_call_tail(tokens, start, ImportPhase::Eval)
}

/// Parse `( AssignmentExpression [ , AssignmentExpression ] [,] )` after the
/// `import` [`phase`] keyword(s).
fn parse_import_call_tail(
    tokens: &mut ParserTokenStream,
    start: Span,
    phase: ImportPhase,
) -> Result<Expr, Vec<Diagnostic>> {
    expect_punctuator(tokens, Punctuator::LParen)?;
    let source = parse_assignment(tokens)?;
    let mut options = None;
    if tokens.eat_punctuator(Punctuator::Comma) {
        if !matches!(
            tokens.peek_kind(),
            TokenKind::Punctuator(Punctuator::RParen)
        ) {
            options = Some(Box::new(parse_assignment(tokens)?));
        }
        // Optional trailing comma after the (possibly absent) options argument.
        tokens.eat_punctuator(Punctuator::Comma);
    }
    let close = expect_punctuator(tokens, Punctuator::RParen)?;
    Ok(Expr::ImportCall {
        span: Span::new(start.start, close.end),
        phase,
        source: Box::new(source),
        options,
    })
}

/// `new` consumed as a primary keyword token (rare; mainly guards re-entry).
fn parse_new_from_primary(
    tokens: &mut ParserTokenStream,
    start: Span,
) -> Result<Expr, Vec<Diagnostic>> {
    if tokens.eat_punctuator(Punctuator::Dot) {
        if let TokenKind::Ident(_) = tokens.peek_kind() {
            tokens.bump();
        }
        return Ok(Expr::Member(Box::new(MemberExpr {
            span: Span::new(start.start, tokens.span().start),
            object: Box::new(Expr::This(start)),
            property: MemberProp::Ident("target".to_string()),
            optional: false,
        })));
    }
    parse_new_expr_after_keyword(tokens, start)
}

fn parse_new_expr_after_keyword(
    tokens: &mut ParserTokenStream,
    start: Span,
) -> Result<Expr, Vec<Diagnostic>> {
    let mut callee = parse_primary(tokens)?;
    loop {
        match tokens.peek_kind() {
            TokenKind::Punctuator(Punctuator::Dot) => {
                tokens.bump();
                let (name, is_private) = parse_property_name(tokens)?;
                let end = tokens.span();
                let prop = if is_private {
                    MemberProp::Private(name)
                } else {
                    MemberProp::Ident(name)
                };
                callee = Expr::Member(Box::new(MemberExpr {
                    span: Span::new(callee.span().start, end.start),
                    object: Box::new(callee),
                    property: prop,
                    optional: false,
                }));
            }
            _ => break,
        }
    }
    let args = if matches!(
        tokens.peek_kind(),
        TokenKind::Punctuator(Punctuator::LParen)
    ) {
        parse_call_args(tokens)?
    } else {
        Vec::new()
    };
    let end = last_arg_end(&args).unwrap_or(callee.span().end);
    Ok(Expr::New(Box::new(NewExpr {
        span: Span::new(start.start, end),
        callee: Box::new(callee),
        args,
    })))
}

/// `[a, b, ...c, ,hole]`.
fn parse_array_literal(
    tokens: &mut ParserTokenStream,
    start: Span,
) -> Result<Expr, Vec<Diagnostic>> {
    let mut elements = Vec::new();
    let mut trailing_comma = false;
    loop {
        if matches!(
            tokens.peek_kind(),
            TokenKind::Punctuator(Punctuator::RBracket)
        ) {
            break;
        }
        if matches!(tokens.peek_kind(), TokenKind::Punctuator(Punctuator::Comma)) {
            tokens.bump();
            elements.push(None); // hole
            continue;
        }
        if tokens.eat_punctuator(Punctuator::Spread) {
            let e = parse_assignment(tokens)?;
            elements.push(Some(ArrayExprElement::Spread(e)));
        } else {
            let e = parse_assignment(tokens)?;
            elements.push(Some(ArrayExprElement::Expr(e)));
        }
        if matches!(tokens.peek_kind(), TokenKind::Punctuator(Punctuator::Comma)) {
            tokens.bump();
            trailing_comma = matches!(
                tokens.peek_kind(),
                TokenKind::Punctuator(Punctuator::RBracket)
            );
        } else {
            break;
        }
    }
    let close = expect_punctuator(tokens, Punctuator::RBracket)?;
    Ok(Expr::Array {
        span: Span::new(start.start, close.end),
        elements,
        trailing_comma,
    })
}

/// `{ a, b: c, [k]: v, ...d, m(){}, get p(){}, set p(x){} }`.
fn parse_object_literal(
    tokens: &mut ParserTokenStream,
    start: Span,
) -> Result<Expr, Vec<Diagnostic>> {
    let mut props = Vec::new();
    while !matches!(
        tokens.peek_kind(),
        TokenKind::Punctuator(Punctuator::RBrace)
    ) {
        // Spread.
        if tokens.eat_punctuator(Punctuator::Spread) {
            let e = parse_assignment(tokens)?;
            props.push(ObjectProp {
                span: e.span(),
                key: PropKey::Ident(String::new()),
                value: ObjectPropValue::Spread(e),
                computed: false,
                method: false,
                shorthand: false,
                kind: ObjectPropKind::Init,
            });
        } else {
            props.push(parse_object_prop(tokens)?);
        }
        if matches!(tokens.peek_kind(), TokenKind::Punctuator(Punctuator::Comma)) {
            tokens.bump();
        } else {
            break;
        }
    }
    let close = expect_punctuator(tokens, Punctuator::RBrace)?;
    Ok(Expr::Object {
        span: Span::new(start.start, close.end),
        props,
    })
}

fn parse_object_prop(tokens: &mut ParserTokenStream) -> Result<ObjectProp, Vec<Diagnostic>> {
    let prop_start = tokens.span();

    // Concise async / generator methods: `async name(){}`, `*name(){}`,
    // `async *name(){}`. (`async` is contextual; it is a modifier only when an
    // async method follows — shared lookahead with class-member parsing.)
    let mut is_async = false;
    if matches!(tokens.peek_kind(), TokenKind::Keyword(Keyword::Async))
        && !tokens.preceded_by_newline_at(1)
        && crate::class::is_async_modifier_ahead(tokens)
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
    let is_generator = tokens.eat_punctuator(Punctuator::Mul);

    // get / set accessors (not combinable with async/generator in valid code).
    let mut kind = ObjectPropKind::Init;
    let mut is_method = false;
    if !is_async && !is_generator {
        if matches!(tokens.peek_kind(), TokenKind::Keyword(Keyword::Get))
            && !is_property_terminator(&tokens.peek2().kind)
        {
            if tokens.current_token_contains_escape() {
                return Err(vec![Diagnostic::error(
                    tokens.span(),
                    "the accessor `get` modifier may not contain an escape sequence",
                )]);
            }
            tokens.bump();
            kind = ObjectPropKind::Get;
            is_method = true;
        } else if matches!(tokens.peek_kind(), TokenKind::Keyword(Keyword::Set))
            && !is_property_terminator(&tokens.peek2().kind)
        {
            if tokens.current_token_contains_escape() {
                return Err(vec![Diagnostic::error(
                    tokens.span(),
                    "the accessor `set` modifier may not contain an escape sequence",
                )]);
            }
            tokens.bump();
            kind = ObjectPropKind::Set;
            is_method = true;
        }
    }

    // Key (possibly computed).
    let (key, computed) = parse_property_key(tokens)?;
    if matches!(key, PropKey::Private(_)) {
        return Err(vec![Diagnostic::error(
            prop_start,
            "an object literal property may not have a private name",
        )]);
    }

    // Method shorthand: `name(params){body}`, `*name(){}`, `async name(){}` …
    if matches!(
        tokens.peek_kind(),
        TokenKind::Punctuator(Punctuator::LParen)
    ) {
        tokens.enter_fn(is_async, is_generator);
        let result = (|| {
            let params = parse_params(tokens)?;
            match kind {
                ObjectPropKind::Get if !params.is_empty() => {
                    return Err(vec![Diagnostic::error(
                        prop_start,
                        "an object getter must not have parameters",
                    )]);
                }
                ObjectPropKind::Set
                    if params.len() != 1 || matches!(params.first(), Some(Pat::Rest { .. })) =>
                {
                    return Err(vec![Diagnostic::error(
                        prop_start,
                        "an object setter must have exactly one non-rest parameter",
                    )]);
                }
                _ => {}
            }
            let (body, close) = parse_block(tokens)?;
            let span = Span::new(prop_start.start, close.end);
            let func = FunctionExpr {
                span,
                name: propkey_name(&key),
                params,
                body,
                is_async,
                is_generator,
            };
            Ok(ObjectProp {
                span,
                key,
                value: ObjectPropValue::Method(Box::new(func)),
                computed,
                method: true,
                shorthand: false,
                kind: if is_method {
                    kind
                } else {
                    ObjectPropKind::Init
                },
            })
        })();
        tokens.pop_ctx();
        return result;
    }

    if is_generator || !matches!(kind, ObjectPropKind::Init) {
        return Err(vec![Diagnostic::error(
            prop_start,
            "a method prefix must be followed by a method definition",
        )]);
    }

    // Normal `key: value`.
    if tokens.eat_punctuator(Punctuator::Colon) {
        let v = parse_assignment(tokens)?;
        let span = Span::new(prop_start.start, v.span().end);
        return Ok(ObjectProp {
            span,
            key,
            value: ObjectPropValue::Expr(v),
            computed,
            method: false,
            shorthand: false,
            kind,
        });
    }

    // Shorthand `{ a }` / `{ a = default }`. A shorthand property names a
    // binding/identifier *reference*, which may NOT be a ReservedWord — so
    // `{ continue }` and `{ continue }` are SyntaxErrors ("IdentifierName
    // but not ReservedWord"). Computed keys (`[...]`) and explicit `key:`
    // forms want an IdentifierName and are exempt. Contextual/strict-only
    // words (let/async/yield/await/…) can be ordinary identifiers in some
    // mode, so they are not unconditionally rejected here.
    let PropKey::Ident(shorthand_name) = &key else {
        return Err(vec![Diagnostic::error(
            prop_start,
            "a shorthand property must be an identifier reference",
        )]);
    };
    if computed {
        return Err(vec![Diagnostic::error(
            prop_start,
            "a computed property name requires a value or method definition",
        )]);
    }
    let shorthand_name = shorthand_name.clone();
    let reserved_bad = Keyword::from_str(&shorthand_name)
        .map(|kw| !shorthand_keyword_allowed(kw))
        .unwrap_or(false);
    if reserved_bad {
        return Err(vec![Diagnostic::error(
            prop_start,
            "a shorthand property name may not be a reserved word",
        )]);
    }
    let default = if tokens.eat_punctuator(Punctuator::Assign) {
        Some(parse_assignment(tokens)?)
    } else {
        None
    };
    let value_expr = match default {
        Some(d) => Expr::Assign {
            span: Span::new(prop_start.start, d.span().end),
            op: AssignOp::Assign,
            left: AssignTarget::Ident {
                span: prop_start,
                name: shorthand_name.clone(),
            },
            right: Box::new(d),
        },
        None => Expr::Ident {
            span: prop_start,
            name: shorthand_name.clone(),
        },
    };
    let span = Span::new(prop_start.start, value_expr.span().end);
    Ok(ObjectProp {
        span,
        key,
        value: ObjectPropValue::Expr(value_expr),
        computed,
        method: false,
        shorthand: true,
        kind: ObjectPropKind::Init,
    })
}

/// Whether the token after `get`/`set` could *not* start a property key, meaning
/// `get`/`set` is itself the property name (shorthand).
fn is_property_terminator(kind: &TokenKind) -> bool {
    matches!(
        kind,
        TokenKind::Punctuator(Punctuator::Comma)
            | TokenKind::Punctuator(Punctuator::RBrace)
            | TokenKind::Punctuator(Punctuator::Colon)
            | TokenKind::Punctuator(Punctuator::LParen)
            | TokenKind::Punctuator(Punctuator::Assign)
    )
}

/// A property key: identifier, string, number, private name, or `[computed]`.
fn parse_property_key(tokens: &mut ParserTokenStream) -> Result<(PropKey, bool), Vec<Diagnostic>> {
    if tokens.eat_punctuator(Punctuator::LBracket) {
        let e = parse_assignment(tokens)?;
        let _ = expect_punctuator(tokens, Punctuator::RBracket)?;
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
            let n = js_lexer::parse_number(&raw).unwrap_or(f64::NAN);
            Ok((PropKey::Number(n), false))
        }
        TokenKind::Bigint(raw) => {
            tokens.bump();
            Ok((PropKey::String(bigint_property_name(&raw)), false))
        }
        other => Err(vec![Diagnostic::error(
            tokens.span(),
            format!("expected property key, found {:?}", other),
        )]),
    }
}

/// Best-effort identifier name for a property key (for shorthand naming).
fn propkey_name(key: &PropKey) -> Option<String> {
    match key {
        PropKey::Ident(n) | PropKey::String(n) | PropKey::Private(n) => Some(n.clone()),
        PropKey::Number(n) => Some(n.to_string()),
        PropKey::Computed(_) => None,
    }
}

pub(crate) fn bigint_property_name(raw: &str) -> String {
    let cleaned = raw.strip_suffix('n').unwrap_or(raw).replace('_', "");
    let (radix, digits) = if let Some(digits) = cleaned
        .strip_prefix("0x")
        .or_else(|| cleaned.strip_prefix("0X"))
    {
        (16, digits)
    } else if let Some(digits) = cleaned
        .strip_prefix("0o")
        .or_else(|| cleaned.strip_prefix("0O"))
    {
        (8, digits)
    } else if let Some(digits) = cleaned
        .strip_prefix("0b")
        .or_else(|| cleaned.strip_prefix("0B"))
    {
        (2, digits)
    } else {
        (10, cleaned.as_str())
    };

    // Little-endian decimal digits. This keeps arbitrarily large property
    // names exact without introducing a BigInt dependency into the AST.
    let mut decimal = vec![0u8];
    for digit in digits.chars() {
        let Some(value) = digit.to_digit(radix) else {
            return cleaned;
        };
        let mut carry = value;
        for place in &mut decimal {
            let next = u32::from(*place) * radix + carry;
            *place = (next % 10) as u8;
            carry = next / 10;
        }
        while carry != 0 {
            decimal.push((carry % 10) as u8);
            carry /= 10;
        }
    }
    while decimal.len() > 1 && decimal.last() == Some(&0) {
        decimal.pop();
    }
    decimal
        .iter()
        .rev()
        .map(|digit| char::from(b'0' + *digit))
        .collect()
}

fn string_contains_legacy_escape(raw: &str) -> bool {
    let mut chars = raw.chars().peekable();
    let _ = chars.next();
    while let Some(character) = chars.next() {
        if character != '\\' {
            continue;
        }
        let Some(escaped) = chars.next() else {
            return false;
        };
        match escaped {
            '1'..='9' => return true,
            '0' if chars.peek().is_some_and(char::is_ascii_digit) => return true,
            '\r' if chars.peek() == Some(&'\n') => {
                chars.next();
            }
            _ => {}
        }
    }
    false
}

/// `PropKey` carries no span of its own; return a dummy for bookkeeping fields.
fn propkey_span(_key: &PropKey) -> Span {
    Span::DUMMY
}

/// A template literal `` `head${expr}...tail` ``. The first chunk was already
/// consumed as the current token; `tail` tells whether it closed the template.
/// For a non-tail chunk, substitution expressions follow, each terminated by the
/// lexer turning the matching `}` into the next chunk.
fn parse_template(
    tokens: &mut ParserTokenStream,
    start: Span,
    raw0: String,
    cooked0: Option<String>,
    tail0: bool,
    tagged: bool,
) -> Result<Expr, Vec<Diagnostic>> {
    if !tagged && cooked0.is_none() {
        return Err(vec![Diagnostic::error(
            start,
            "invalid escape sequence in untagged template literal",
        )]);
    }
    if tail0 {
        return Ok(Expr::TemplateLit {
            span: start,
            quasis: vec![(cooked0, raw0)],
            expressions: Vec::new(),
        });
    }
    let mut quasis = vec![(cooked0, raw0)];
    let mut expressions = Vec::new();
    let mut end;
    loop {
        let e = parse_expression(tokens)?;
        expressions.push(e);
        // The lexer emits the substitution-closing `}` as a regular RBrace
        // (kept separate from the template continuation chunk to avoid
        // ambiguity with tagged-template literals).
        expect_punctuator(tokens, Punctuator::RBrace)?;
        let nt = tokens.bump();
        match nt.kind {
            TokenKind::Template { raw, cooked, tail } => {
                if !tagged && cooked.is_none() {
                    return Err(vec![Diagnostic::error(
                        nt.span,
                        "invalid escape sequence in untagged template literal",
                    )]);
                }
                end = nt.span.end;
                quasis.push((cooked, raw));
                if tail {
                    break;
                }
            }
            other => {
                return Err(vec![Diagnostic::error(
                    nt.span,
                    format!("expected template chunk, found {:?}", other),
                )]);
            }
        }
    }
    Ok(Expr::TemplateLit {
        span: Span::new(start.start, end),
        quasis,
        expressions,
    })
}

/// `function name?(params){body}` expression. `is_async` covers the
/// `async function` prefix; a leading `*` makes it a generator.
fn parse_function_expr(
    tokens: &mut ParserTokenStream,
    start: Span,
    is_async: bool,
) -> Result<Expr, Vec<Diagnostic>> {
    let is_generator = tokens.eat_punctuator(Punctuator::Mul);
    tokens.enter_fn(is_async, is_generator);
    let result = (|| {
        let name = binding_identifier(tokens);
        let params = parse_params(tokens)?;
        let (body, close) = parse_block(tokens)?;
        let span = Span::new(start.start, close.end);
        Ok(Expr::Function(Box::new(FunctionExpr {
            span,
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

/// Consume a BindingIdentifier: an `Ident`, or a contextual keyword used as a
/// name. `await` is a valid identifier only outside async contexts (and module
/// top level); `yield` only outside generators; `async`/`of`/`from`/`as`/`get`/
/// `set` are purely contextual and always allowed. Strict-mode-only reserved
/// words (`let`, `static`, `implements`, …) are NOT accepted here — they need
/// strict-mode tracking, which the parser does not yet do. Returns `None`
/// (consuming nothing) when the token can't be a binding name.
/// Whether a keyword may serve as a binding identifier in the given context,
/// returning its spelling if so (`await` outside async, `yield` outside
/// generators, the always-contextual words). Strict-mode-only reserved words
/// (`let`, `static`, …) are excluded (need strict tracking).
pub(crate) fn keyword_binding_name(
    kw: Keyword,
    ctx: crate::token_stream::FnCtx,
) -> Option<&'static str> {
    let allowed = match kw {
        Keyword::Await => !ctx.is_async,
        Keyword::Yield => !ctx.is_generator,
        Keyword::Static => !ctx.is_strict,
        // `let` remains excluded because statement/declaration lookahead needs
        // dedicated grammar handling. `static` is unambiguous in a binding
        // position and post-parse Early Errors cover directive strictness.
        Keyword::Let
        | Keyword::Async
        | Keyword::Of
        | Keyword::From
        | Keyword::As
        | Keyword::Get
        | Keyword::Set
        | Keyword::Undefined => true,
        _ => false,
    };
    if allowed {
        Some(kw.as_str())
    } else {
        None
    }
}

/// Peek the current token as a binding-identifier name without consuming it.
pub(crate) fn peek_binding_name(tokens: &ParserTokenStream) -> Option<String> {
    match tokens.peek_kind().clone() {
        TokenKind::Ident(n) => Some(n),
        TokenKind::Keyword(k) => {
            keyword_binding_name(k, tokens.current_ctx()).map(|s| s.to_string())
        }
        _ => None,
    }
}

pub(crate) fn binding_identifier(tokens: &mut ParserTokenStream) -> Option<String> {
    match tokens.peek_kind().clone() {
        TokenKind::Ident(n) => {
            tokens.bump();
            Some(n)
        }
        TokenKind::Keyword(k) => {
            let name = keyword_binding_name(k, tokens.current_ctx()).map(|s| s.to_string());
            if name.is_some() {
                tokens.bump();
            }
            name
        }
        _ => None,
    }
}

/// `(a, b, ...rest)` — parameter list.
pub(crate) fn parse_params(tokens: &mut ParserTokenStream) -> Result<Vec<Pat>, Vec<Diagnostic>> {
    let _ = expect_punctuator(tokens, Punctuator::LParen)?;
    let mut params = Vec::new();
    if !matches!(
        tokens.peek_kind(),
        TokenKind::Punctuator(Punctuator::RParen)
    ) {
        loop {
            if tokens.eat_punctuator(Punctuator::Spread) {
                let p = parse_binding_pattern(tokens)?;
                params.push(Pat::Rest {
                    span: p.span(),
                    arg: Box::new(p),
                });
                if !matches!(
                    tokens.peek_kind(),
                    TokenKind::Punctuator(Punctuator::RParen)
                ) {
                    return Err(vec![Diagnostic::error(
                        tokens.span(),
                        "a rest parameter must be last and may not have a trailing comma",
                    )]);
                }
                break;
            } else {
                params.push(parse_binding_pattern(tokens)?);
            }
            if !tokens.eat_punctuator(Punctuator::Comma) {
                break;
            }
            // Trailing comma: `f(a, b,)` — end the list.
            if matches!(
                tokens.peek_kind(),
                TokenKind::Punctuator(Punctuator::RParen)
            ) {
                break;
            }
        }
    }
    let _ = expect_punctuator(tokens, Punctuator::RParen)?;
    Ok(params)
}

/// A binding pattern *without* a trailing default. Used by `var`/`let`/`const`
/// declarators, where the `= init` belongs to the declarator, not the pattern.
/// (Defaults *inside* nested `[...]`/`{...}` are still parsed.)
pub(crate) fn parse_binding_target(tokens: &mut ParserTokenStream) -> Result<Pat, Vec<Diagnostic>> {
    let span = tokens.span();
    if let Some(name) = binding_identifier(tokens) {
        return Ok(Pat::Ident { span, name });
    }
    match tokens.peek_kind().clone() {
        TokenKind::Punctuator(Punctuator::LBracket) => parse_array_pattern(tokens),
        TokenKind::Punctuator(Punctuator::LBrace) => parse_object_pattern(tokens),
        other => Err(vec![Diagnostic::error(
            tokens.span(),
            format!("expected binding pattern, found {:?}", other),
        )]),
    }
}

/// A binding pattern with an optional default `= expr`. Used by function params,
/// arrow params, and `catch`.
pub(crate) fn parse_binding_pattern(
    tokens: &mut ParserTokenStream,
) -> Result<Pat, Vec<Diagnostic>> {
    let mut pat = parse_binding_target(tokens)?;
    // Default value: `x = expr`.
    if tokens.eat_punctuator(Punctuator::Assign) {
        let default = parse_assignment(tokens)?;
        let span = Span::new(pat.span().start, default.span().end);
        pat = Pat::Assignment {
            span,
            left: Box::new(pat),
            right: Box::new(default),
        };
    }
    Ok(pat)
}

fn parse_array_pattern(tokens: &mut ParserTokenStream) -> Result<Pat, Vec<Diagnostic>> {
    let start = tokens.span();
    tokens.bump(); // `[`
    let mut elements = Vec::new();
    while !matches!(
        tokens.peek_kind(),
        TokenKind::Punctuator(Punctuator::RBracket)
    ) {
        if matches!(tokens.peek_kind(), TokenKind::Punctuator(Punctuator::Comma)) {
            tokens.bump();
            elements.push(Some(ArrayPatElement::Hole(tokens.span())));
            continue;
        }
        if tokens.eat_punctuator(Punctuator::Spread) {
            let p = parse_binding_pattern(tokens)?;
            elements.push(Some(ArrayPatElement::Pat(Pat::Rest {
                span: p.span(),
                arg: Box::new(p),
            })));
            break;
        }
        let p = parse_binding_pattern(tokens)?;
        elements.push(Some(ArrayPatElement::Pat(p)));
        if matches!(tokens.peek_kind(), TokenKind::Punctuator(Punctuator::Comma)) {
            tokens.bump();
        } else {
            break;
        }
    }
    let close = expect_punctuator(tokens, Punctuator::RBracket)?;
    Ok(Pat::Array {
        span: Span::new(start.start, close.end),
        elements,
    })
}

fn parse_object_pattern(tokens: &mut ParserTokenStream) -> Result<Pat, Vec<Diagnostic>> {
    let start = tokens.span();
    tokens.bump(); // `{`
    let mut properties = Vec::new();
    while !matches!(
        tokens.peek_kind(),
        TokenKind::Punctuator(Punctuator::RBrace)
    ) {
        if tokens.eat_punctuator(Punctuator::Spread) {
            let p = parse_binding_pattern(tokens)?;
            properties.push(ObjectPatProp::Rest {
                span: p.span(),
                arg: Box::new(p),
            });
            break;
        }
        let (key, computed) = parse_property_key(tokens)?;
        let value = if tokens.eat_punctuator(Punctuator::Colon) {
            parse_binding_pattern(tokens)?
        } else {
            // Shorthand: the key is itself the binding name. A BindingIdentifier
            // may not be a ReservedWord, so `{ continue }` in a destructuring
            // pattern is a SyntaxError (computed keys exempt).
            let name = propkey_name(&key).unwrap_or_default();
            let span = propkey_span(&key);
            if !computed {
                let reserved_bad = Keyword::from_str(&name)
                    .map(|kw| !shorthand_keyword_allowed(kw))
                    .unwrap_or(false);
                if reserved_bad {
                    return Err(vec![Diagnostic::error(
                        span,
                        "a shorthand binding name may not be a reserved word",
                    )]);
                }
            }
            let mut p = Pat::Ident { span, name };
            if tokens.eat_punctuator(Punctuator::Assign) {
                let d = parse_assignment(tokens)?;
                p = Pat::Assignment {
                    span: Span::new(span.start, d.span().end),
                    left: Box::new(p),
                    right: Box::new(d),
                };
            }
            p
        };
        properties.push(ObjectPatProp::KeyValue {
            span: propkey_span(&key),
            key,
            value,
        });
        let _ = computed;
        if matches!(tokens.peek_kind(), TokenKind::Punctuator(Punctuator::Comma)) {
            tokens.bump();
        } else {
            break;
        }
    }
    let close = expect_punctuator(tokens, Punctuator::RBrace)?;
    Ok(Pat::Object {
        span: Span::new(start.start, close.end),
        properties,
    })
}

/// Reinterpret an array/object literal expression as an assignment-target
/// pattern (for destructuring assignment `[a, b] = x` / `{ a, b: c } = x`).
/// Nested patterns, defaults (`a = 1`), holes (`[a, , b]`), and rests
/// (`...r`) are handled. Member targets inside a pattern (`[a.b] = x`) are
/// syntactically valid but not represented by [`Pat`] and are rejected.
fn array_or_object_to_pat(expr: &Expr) -> Result<Pat, Diagnostic> {
    match expr {
        Expr::Array {
            span,
            elements,
            trailing_comma,
        } => {
            if *trailing_comma && matches!(elements.last(), Some(Some(ArrayExprElement::Spread(_))))
            {
                return Err(Diagnostic::error(
                    *span,
                    "an assignment rest element may not have a trailing comma",
                ));
            }
            let mut out = Vec::with_capacity(elements.len());
            for (idx, el) in elements.iter().enumerate() {
                match el {
                    None => out.push(None), // hole `,`
                    Some(ArrayExprElement::Spread(e)) => {
                        // `...rest` — must be the final element.
                        if idx != elements.len() - 1 {
                            return Err(Diagnostic::error(
                                e.span(),
                                "rest element must be last in an array pattern",
                            ));
                        }
                        let arg = expr_to_assignment_pat(e)?;
                        out.push(Some(ArrayPatElement::Pat(Pat::Rest {
                            span: e.span(),
                            arg: Box::new(arg),
                        })));
                    }
                    Some(ArrayExprElement::Expr(e)) => {
                        out.push(Some(ArrayPatElement::Pat(expr_to_assignment_pat(e)?)));
                    }
                }
            }
            Ok(Pat::Array {
                span: *span,
                elements: out,
            })
        }
        Expr::Object { span, props } => {
            let mut properties = Vec::with_capacity(props.len());
            for (idx, p) in props.iter().enumerate() {
                match &p.value {
                    ObjectPropValue::Spread(e) => {
                        // `...rest` — must be the final property.
                        if idx != props.len() - 1 {
                            return Err(Diagnostic::error(
                                p.span,
                                "rest property must be last in an object pattern",
                            ));
                        }
                        let arg = Box::new(expr_to_assignment_pat(e)?);
                        properties.push(ObjectPatProp::Rest { span: p.span, arg });
                    }
                    ObjectPropValue::Expr(v) => {
                        let value = expr_to_assignment_pat(v)?;
                        properties.push(ObjectPatProp::KeyValue {
                            span: p.span,
                            key: p.key.clone(),
                            value,
                        });
                    }
                    ObjectPropValue::Method(_) => {
                        return Err(Diagnostic::error(
                            p.span,
                            "a method cannot be a destructuring target",
                        ));
                    }
                }
            }
            Ok(Pat::Object {
                span: *span,
                properties,
            })
        }
        _ => Err(Diagnostic::error(
            expr.span(),
            "not a destructuring pattern",
        )),
    }
}

/// Reinterpret a for-in/of expression head using AssignmentPattern as its
/// grammar goal. Conversion failures are syntax errors, never placeholder
/// targets.
pub(crate) fn assignment_pattern_from_expr(expr: &Expr) -> Result<Pat, Diagnostic> {
    match expr {
        Expr::Paren { expr, .. }
            if matches!(expr.as_ref(), Expr::Ident { .. } | Expr::Member(_)) =>
        {
            assignment_pattern_from_expr(expr)
        }
        Expr::Ident { span, name } => Ok(Pat::Ident {
            span: *span,
            name: name.clone(),
        }),
        Expr::Member(member) => Ok(Pat::Member(member.clone())),
        Expr::Array { .. } | Expr::Object { .. } => array_or_object_to_pat(expr),
        _ => Err(Diagnostic::error(
            expr.span(),
            "invalid for-in/of assignment target",
        )),
    }
}

/// Convert an expression that appears in an assignment-target position into a
/// [`Pat`]: identifier, nested array/object pattern, or a defaulted target
/// (`x = default`). Plain member expressions (`a.b`) are not patterns and are
/// rejected (they are valid assignment targets, but [`Pat`] cannot represent
/// them).
fn expr_to_assignment_pat(e: &Expr) -> Result<Pat, Diagnostic> {
    match e {
        Expr::Ident { span, name } => Ok(Pat::Ident {
            span: *span,
            name: name.clone(),
        }),
        Expr::Array { .. } | Expr::Object { .. } => array_or_object_to_pat(e),
        Expr::Member(m) => Ok(Pat::Member(m.clone())),
        Expr::Assign {
            span,
            op: AssignOp::Assign,
            left,
            right,
        } => {
            let lp = assign_target_to_pat(left)?;
            Ok(Pat::Assignment {
                span: *span,
                left: Box::new(lp),
                right: right.clone(),
            })
        }
        _ => Err(Diagnostic::error(e.span(), "invalid destructuring target")),
    }
}

/// Convert an [`AssignTarget`] (the LHS of an already-parsed `=` expression)
/// into a [`Pat`], for nested defaults like `[a = 1, b = 2] = x`.
fn assign_target_to_pat(t: &AssignTarget) -> Result<Pat, Diagnostic> {
    match t {
        AssignTarget::Ident { span, name } => Ok(Pat::Ident {
            span: *span,
            name: name.clone(),
        }),
        AssignTarget::Pat(p) => Ok(p.clone()),
        AssignTarget::Member(m) => Ok(Pat::Member(m.clone())),
    }
}

/// Parse a `{ ... }` block, returning the body and the closing-brace span.
pub(crate) fn parse_block(
    tokens: &mut ParserTokenStream,
) -> Result<(Vec<Stmt>, Span), Vec<Diagnostic>> {
    let _open = expect_punctuator(tokens, Punctuator::LBrace)?;
    let mut body = Vec::new();
    let mut errors = Vec::new();
    while !matches!(
        tokens.peek_kind(),
        TokenKind::Punctuator(Punctuator::RBrace) | TokenKind::Eof
    ) {
        match crate::stmt::parse_statement_list_item(tokens) {
            Ok(s) => body.push(s),
            Err(diags) => {
                errors.extend(diags);
                recover_to_statement_boundary(tokens);
            }
        }
    }
    let close = expect_punctuator(tokens, Punctuator::RBrace)?;
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

/// Consume a statement terminator, implementing Automatic Semicolon Insertion
/// (ES2024 12.9). A semicolon is considered present when:
///   1. an explicit `;` is consumed, or
///   2. the upcoming token is `}` or EOF, or
///   3. a line terminator preceded the upcoming token.
/// Otherwise the statement is unterminated → SyntaxError.
pub(crate) fn consume_asi(tokens: &mut ParserTokenStream) -> Result<(), Vec<Diagnostic>> {
    if tokens.eat_punctuator(Punctuator::Semicolon) {
        return Ok(());
    }
    let at_boundary = matches!(
        tokens.peek_kind(),
        TokenKind::Eof | TokenKind::Punctuator(Punctuator::RBrace)
    );
    if at_boundary || tokens.preceded_by_newline() {
        Ok(())
    } else {
        Err(vec![Diagnostic::error(
            tokens.span(),
            "expected `;` or a line terminator before this token",
        )])
    }
}

/// Error recovery shared with statement parsing.
pub(crate) fn recover_to_statement_boundary(tokens: &mut ParserTokenStream) {
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
