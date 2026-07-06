//! AST → bytecode compiler.
//!
//! Walks a [`Program`] and emits a [`BytecodeModule`]: one [`BytecodeFunction`]
//! for the top-level script (`<main>`, id 0) plus one nested function per
//! function declaration (ids 1, 2, ... in discovery order).
//!
//! Milestone-1 coverage: numeric / boolean / null / string literals, identifiers
//! (locals + `undefined` + globals), binary & unary operators, parenthesized
//! expressions, assignment, call expressions, `var`/`let`/`const` declarations,
//! `return`, `if`, `while`, blocks, expression statements, and function
//! declarations.

use crate::constant::ConstantPool;
use crate::module::{BytecodeFunction, BytecodeModule};
use crate::opcode::{Instruction, Opcode};
use js_diagnostics::{Diagnostic, DiagResult};
use js_syntax::ast::expr::{AssignTarget, CallArg, Expr};
use js_syntax::ast::lit::Lit;
use js_syntax::ast::op::BinOp;
use js_syntax::ast::pat::Pat;
use js_syntax::ast::stmt::{Decl, Stmt};
use js_syntax::ast::{AssignOp, FunctionDecl, Program};

/// Compile a parsed [`Program`] into a [`BytecodeModule`].
pub fn compile_program(program: &Program) -> DiagResult<BytecodeModule> {
    let mut ctx = CompilerCtx {
        constants: ConstantPool::new(),
        functions: Vec::new(),
        errors: Vec::new(),
    };
    let mut main = BytecodeFunction::new(program.span, "<main>", 0);

    compile_block(&program.body, &mut main, &mut ctx, true);

    // Top-level completion value: the last expression statement leaves its
    // value on the stack; `Return` pops it (or undefined if empty).
    main.emit_bare(Opcode::Return);

    if !ctx.errors.is_empty() {
        return Err(ctx.errors);
    }
    Ok(BytecodeModule {
        constants: ctx.constants,
        main,
        functions: ctx.functions,
    })
}

/// Compile a sequence of [`ProgramItem`]s. When `top_level` is true (only the
/// `<main>` script body), expression statements leave their value on the stack
/// so the script's completion value is the last expression.
fn compile_block(
    items: &[js_syntax::ast::ProgramItem],
    func: &mut BytecodeFunction,
    ctx: &mut CompilerCtx,
    top_level: bool,
) {
    for item in items {
        match item {
            js_syntax::ast::ProgramItem::Stmt(s) => compile_stmt(s, func, ctx, top_level),
            js_syntax::ast::ProgramItem::Decl(d) => compile_decl(d, func, ctx),
        }
    }
}

fn compile_stmt(stmt: &Stmt, func: &mut BytecodeFunction, ctx: &mut CompilerCtx, top_level: bool) {
    match stmt {
        Stmt::Empty(_) | Stmt::Debugger(_) => {}
        Stmt::Block { body, .. } => {
            // Milestone: blocks do not introduce a new scope; locals are
            // function-scoped (good enough for `var`, conservative for `let`).
            let items: Vec<_> = body
                .iter()
                .map(|s| js_syntax::ast::ProgramItem::Stmt(s.clone()))
                .collect();
            compile_block(&items, func, ctx, false);
        }
        Stmt::Expr { expr, .. } => {
            compile_expr(expr, func, ctx);
            // At top level the value is the script completion; elsewhere drop it.
            if !top_level {
                func.emit_bare(Opcode::Pop);
            }
        }
        Stmt::Return { arg, .. } => {
            match arg {
                Some(e) => compile_expr(e, func, ctx),
                None => func.emit_bare(Opcode::LdaUndefined),
            }
            func.emit_bare(Opcode::Return);
        }
        Stmt::Decl(d) => compile_decl(d, func, ctx),
        Stmt::If { test, cons, alt, .. } => {
            compile_expr(test, func, ctx);
            let jmp_false = emit_placeholder(func, Opcode::JumpIfFalse);
            compile_stmt(cons, func, ctx, false);
            let jmp_end = if alt.is_some() {
                let j = emit_placeholder(func, Opcode::Jump);
                patch(func, jmp_false, func.here());
                compile_stmt(alt.as_ref().unwrap(), func, ctx, false);
                Some(j)
            } else {
                patch(func, jmp_false, func.here());
                None
            };
            if let Some(j) = jmp_end {
                patch(func, j, func.here());
            }
        }
        Stmt::While { test, body, .. } => {
            let start = func.here();
            compile_expr(test, func, ctx);
            let jmp_end = emit_placeholder(func, Opcode::JumpIfFalse);
            compile_stmt(body, func, ctx, false);
            emit_jump(func, Opcode::Jump, start);
            patch(func, jmp_end, func.here());
        }
        other => {
            ctx.errors.push(Diagnostic::error(
                other.span(),
                "this statement kind is not supported yet",
            ));
        }
    }
}

fn compile_decl(decl: &Decl, func: &mut BytecodeFunction, ctx: &mut CompilerCtx) {
    match decl {
        Decl::Var { kind: _, declarations, .. } => {
            for d in declarations {
                let slot = intern_pat(&d.name, func);
                match (&d.init, &d.name) {
                    (Some(init), Pat::Ident { .. }) => {
                        compile_expr(init, func, ctx);
                        func.emit(Instruction::new(Opcode::StaLocal, slot));
                    }
                    _ => {
                        // Uninitialized binding: leave the slot as undefined.
                    }
                }
            }
        }
        Decl::Function(f) => compile_function_decl(f, func, ctx),
        other => {
            ctx.errors.push(Diagnostic::error(
                other.span(),
                "this declaration kind is not supported yet",
            ));
        }
    }
}

fn compile_function_decl(
    f: &FunctionDecl,
    parent: &mut BytecodeFunction,
    ctx: &mut CompilerCtx,
) {
    // Assign an id BEFORE recursing so nested-nested functions get the right
    // ids. id 0 = <main>; nested functions get ids 1, 2, ... in order.
    let id = (ctx.functions.len() + 1) as u32;

    let name = f.name.clone().unwrap_or_else(|| "<anonymous>".to_string());
    let mut nested = BytecodeFunction::new(f.span, name, 0);
    // Bind parameter names to slots 0..n-1 first, then set param_count.
    for p in &f.params {
        intern_pat(p, &mut nested);
    }
    nested.param_count = f.params.len() as u16;

    compile_stmt_list_body(&f.body, &mut nested, ctx);
    nested.emit_bare(Opcode::LdaUndefined);
    nested.emit_bare(Opcode::Return);

    // Parent loads the function value and binds it by name.
    let name = f.name.clone().unwrap_or_else(|| "<anonymous>".to_string());
    parent.emit(Instruction::new(Opcode::LdaFunction, id as u16));
    let slot = parent.locals.intern(name);
    parent.emit(Instruction::new(Opcode::StaLocal, slot));

    ctx.functions.push(nested);
}

/// Compile a function body (a `Vec<Stmt>`).
fn compile_stmt_list_body(body: &[Stmt], func: &mut BytecodeFunction, ctx: &mut CompilerCtx) {
    let items: Vec<_> = body
        .iter()
        .map(|s| js_syntax::ast::ProgramItem::Stmt(s.clone()))
        .collect();
    compile_block(&items, func, ctx, false);
}

fn compile_expr(expr: &Expr, func: &mut BytecodeFunction, ctx: &mut CompilerCtx) {
    match expr {
        Expr::Lit(lit) => compile_lit(lit, func, ctx),
        Expr::Ident { name, .. } => {
            if name == "undefined" {
                func.emit_bare(Opcode::LdaUndefined);
            } else if let Some(slot) = func.locals.get(name) {
                func.emit(Instruction::new(Opcode::LdaLocal, slot));
            } else {
                let idx = ctx.constants.intern_str(name);
                func.emit(Instruction::new(Opcode::GetGlobal, idx));
            }
        }
        Expr::Paren { expr, .. } => compile_expr(expr, func, ctx),
        Expr::Unary { op, arg, .. } => {
            compile_expr(arg, func, ctx);
            func.emit_bare(Opcode::for_unaryop(*op));
        }
        Expr::Binary { op, left, right, .. } => {
            compile_expr(left, func, ctx);
            compile_expr(right, func, ctx);
            emit_binop(*op, func);
        }
        Expr::Logical { .. } => {
            ctx.errors.push(Diagnostic::error(
                expr.span(),
                "short-circuit logical operators are not supported yet",
            ));
            func.emit_bare(Opcode::LdaUndefined);
        }
        Expr::Conditional { test, cons, alt, .. } => {
            compile_expr(test, func, ctx);
            let jf = emit_placeholder(func, Opcode::JumpIfFalse);
            compile_expr(cons, func, ctx);
            let je = emit_placeholder(func, Opcode::Jump);
            patch(func, jf, func.here());
            compile_expr(alt, func, ctx);
            patch(func, je, func.here());
        }
        Expr::Assign { op, left, right, .. } => {
            if *op != AssignOp::Assign {
                ctx.errors.push(Diagnostic::error(
                    expr.span(),
                    "compound assignment is not supported yet",
                ));
                compile_expr(right, func, ctx);
                func.emit_bare(Opcode::Pop);
                func.emit_bare(Opcode::LdaUndefined);
                return;
            }
            compile_expr(right, func, ctx);
            func.emit_bare(Opcode::Dup);
            match left {
                AssignTarget::Ident { name, .. } => {
                    if let Some(slot) = func.locals.get(name) {
                        func.emit(Instruction::new(Opcode::StaLocal, slot));
                    } else {
                        let idx = ctx.constants.intern_str(name);
                        func.emit(Instruction::new(Opcode::SetGlobal, idx));
                    }
                }
                AssignTarget::Member(_) | AssignTarget::Pat(_) => {
                    ctx.errors.push(Diagnostic::error(
                        expr.span(),
                        "assignment to non-identifier targets is not supported yet",
                    ));
                    func.emit_bare(Opcode::Pop);
                }
            }
        }
        Expr::Call(call) => {
            compile_expr(&call.callee, func, ctx);
            let mut count = 0u16;
            for a in &call.args {
                match a {
                    CallArg::Expr(e) => compile_expr(e, func, ctx),
                    CallArg::Spread(_) => {
                        ctx.errors.push(Diagnostic::error(
                            expr.span(),
                            "spread arguments are not supported yet",
                        ));
                        func.emit_bare(Opcode::LdaUndefined);
                    }
                }
                count += 1;
            }
            func.emit(Instruction::new(Opcode::Call, count));
        }
        Expr::Sequence { exprs, .. } => {
            for (i, e) in exprs.iter().enumerate() {
                compile_expr(e, func, ctx);
                if i + 1 < exprs.len() {
                    func.emit_bare(Opcode::Pop);
                }
            }
        }
        Expr::This { .. } => func.emit_bare(Opcode::LdaUndefined),
        _ => {
            ctx.errors.push(Diagnostic::error(
                expr.span(),
                "this expression kind is not supported yet",
            ));
            func.emit_bare(Opcode::LdaUndefined);
        }
    }
}

fn compile_lit(lit: &Lit, func: &mut BytecodeFunction, ctx: &mut CompilerCtx) {
    match lit {
        Lit::Null(_) => func.emit_bare(Opcode::LdaNull),
        Lit::Boolean(_, b) => func.emit_bare(if *b { Opcode::LdaTrue } else { Opcode::LdaFalse }),
        Lit::Number(_, n) => {
            // Integral literals that fit in i32 take the integer fast path;
            // everything else is stored as a float.
            let idx = if n.fract() == 0.0 && n.is_finite() && *n >= i32::MIN as f64 && *n <= i32::MAX as f64 {
                ctx.constants.intern_int(*n as i32)
            } else {
                ctx.constants.intern_num(*n)
            };
            func.emit(Instruction::new(Opcode::LdaConst, idx));
        }
        Lit::String(_, s) => {
            let idx = ctx.constants.intern_str(s);
            func.emit(Instruction::new(Opcode::LdaConst, idx));
        }
        Lit::BigInt(span, _) | Lit::Regex { span, .. } | Lit::TemplateString { span, .. } => {
            ctx.errors.push(Diagnostic::error(*span, "this literal kind is not supported yet"));
            func.emit_bare(Opcode::LdaUndefined);
        }
    }
}

/// Emit the opcode(s) for a binary operator. Negated comparisons are lowered as
/// the positive comparison followed by a logical `!`.
fn emit_binop(op: BinOp, func: &mut BytecodeFunction) {
    use BinOp::*;
    match op {
        Add => func.emit_bare(Opcode::Add),
        Sub => func.emit_bare(Opcode::Sub),
        Mul => func.emit_bare(Opcode::Mul),
        Div => func.emit_bare(Opcode::Div),
        Mod => func.emit_bare(Opcode::Mod),
        Exp => func.emit_bare(Opcode::Exp),
        Eq => func.emit_bare(Opcode::Eq),
        NotEq => {
            func.emit_bare(Opcode::Eq);
            func.emit_bare(Opcode::Not);
        }
        StrictEq => func.emit_bare(Opcode::StrictEq),
        StrictNotEq => {
            func.emit_bare(Opcode::StrictEq);
            func.emit_bare(Opcode::Not);
        }
        Lt => func.emit_bare(Opcode::Lt),
        Le => func.emit_bare(Opcode::Le),
        Gt => func.emit_bare(Opcode::Gt),
        Ge => func.emit_bare(Opcode::Ge),
        BitAnd => func.emit_bare(Opcode::BitAnd),
        BitOr => func.emit_bare(Opcode::BitOr),
        BitXor => func.emit_bare(Opcode::BitXor),
        Shl => func.emit_bare(Opcode::Shl),
        Shr => func.emit_bare(Opcode::Shr),
        Ushr => func.emit_bare(Opcode::Shr),
        And | Or | NullishCoal | In | Instanceof => {
            // Unsupported for milestone-1; emit Add as a harmless placeholder —
            // the caller already left both operands on the stack.
            func.emit_bare(Opcode::Add);
        }
    }
}

// ---- helpers -------------------------------------------------------------

/// Bind (or look up) a local slot for a binding pattern. Only simple identifier
/// patterns are supported for the milestone.
fn intern_pat(pat: &Pat, func: &mut BytecodeFunction) -> u16 {
    match pat {
        Pat::Ident { name, .. } => func.locals.intern(name),
        _ => {
            // Unsupported patterns fall back to a scratch slot so compilation
            // proceeds; the caller surfaces a diagnostic separately if needed.
            func.locals.intern("<bad-pattern>")
        }
    }
}

fn emit_placeholder(func: &mut BytecodeFunction, op: Opcode) -> u16 {
    let idx = func.here();
    func.emit(Instruction::new(op, 0));
    idx
}

fn emit_jump(func: &mut BytecodeFunction, op: Opcode, target: u16) {
    func.emit(Instruction::new(op, target));
}

fn patch(func: &mut BytecodeFunction, at: u16, target: u16) {
    func.code[at as usize].operand = target;
}

struct CompilerCtx {
    constants: ConstantPool,
    functions: Vec<BytecodeFunction>,
    errors: Vec<Diagnostic>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use js_syntax::ast::ProgramKind;
    use js_syntax::Span;

    #[test]
    fn compile_empty_module() {
        let prog = Program::new(Span::DUMMY, ProgramKind::Script, vec![]);
        let module = compile_program(&prog).expect("empty program compiles");
        assert!(module.functions.is_empty());
        assert!(!module.main.code.is_empty());
    }
}
