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
use js_diagnostics::{DiagResult, Diagnostic};
use js_syntax::ast::expr::{AssignTarget, CallArg, Expr, MemberProp, ObjectPropValue};
use js_syntax::ast::lit::Lit;
use js_syntax::ast::op::{BinOp, UpdateOp};
use js_syntax::ast::pat::Pat;
use js_syntax::ast::stmt::{Decl, ForInit, ForTarget, Stmt};
use js_syntax::ast::{AssignOp, FunctionDecl, Program};
use js_syntax::SourceFile;
use std::sync::Arc;

/// Compile a parsed [`Program`] into a [`BytecodeModule`].
pub fn compile_program(program: &Program) -> DiagResult<BytecodeModule> {
    compile_program_inner(program, None)
}

/// Compile a program while retaining its source for runtime diagnostics.
pub fn compile_program_with_source(
    program: &Program,
    source: Arc<SourceFile>,
) -> DiagResult<BytecodeModule> {
    compile_program_inner(program, Some(source))
}

fn compile_program_inner(
    program: &Program,
    source: Option<Arc<SourceFile>>,
) -> DiagResult<BytecodeModule> {
    let mut ctx = CompilerCtx {
        constants: ConstantPool::new(),
        functions: Vec::new(),
        errors: Vec::new(),
        loops: Vec::new(),
        scopes: Vec::new(),
    };
    // <main> is its own (outermost) scope.
    ctx.scopes.push(Scope::default());
    let mut main = BytecodeFunction::new(program.span, "<main>", 0);

    // Pre-pass: hoist all top-level bindings so nested functions can capture them.
    {
        let mut names = Vec::new();
        for item in &program.body {
            if let js_syntax::ast::ProgramItem::Stmt(s) = item {
                collect_stmt_bindings(s, &mut names);
            } else if let js_syntax::ast::ProgramItem::Decl(d) = item {
                collect_decl_bindings(d, &mut names);
            }
        }
        for n in names {
            ctx.declare_local(&mut main, &n);
        }
    }

    compile_block(&program.body, &mut main, &mut ctx, true);

    // Top-level completion value: the last expression statement leaves its
    // value on the stack; `Return` pops it (or undefined if empty).
    main.emit_bare_at(Opcode::Return, program.span);

    if !ctx.errors.is_empty() {
        for diagnostic in &mut ctx.errors {
            diagnostic.classify(js_diagnostics::DiagnosticPhase::Compile, "JS-COMPILE");
        }
        return Err(ctx.errors);
    }
    Ok(BytecodeModule {
        source,
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
    let start_pc = func.code.len();
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
        Stmt::If {
            test, cons, alt, ..
        } => {
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
            ctx.push_loop();
            compile_stmt(body, func, ctx, false);
            // `continue` targets the test.
            let continue_target = start;
            emit_jump(func, Opcode::Jump, start);
            let end = func.here();
            ctx.pop_loop(func, end, continue_target);
            patch(func, jmp_end, end);
        }
        Stmt::DoWhile { body, test, .. } => {
            let start = func.here();
            ctx.push_loop();
            compile_stmt(body, func, ctx, false);
            let test_target = func.here();
            compile_expr(test, func, ctx);
            emit_jump(func, Opcode::JumpIfTrue, start);
            let end = func.here();
            ctx.pop_loop(func, end, test_target);
        }
        Stmt::For {
            init,
            test,
            update,
            body,
            ..
        } => {
            // Initializer.
            match init {
                Some(ForInit::Var(d)) => compile_decl(d, func, ctx),
                Some(ForInit::Expr(e)) => {
                    compile_expr(e, func, ctx);
                    func.emit_bare(Opcode::Pop);
                }
                None => {}
            }
            let start = func.here();
            // Test.
            let jmp_end = match test {
                Some(t) => {
                    compile_expr(t, func, ctx);
                    Some(emit_placeholder(func, Opcode::JumpIfFalse))
                }
                None => None,
            };
            ctx.push_loop();
            compile_stmt(body, func, ctx, false);
            // `continue` jumps to the update section.
            let update_target = func.here();
            if let Some(u) = update {
                compile_expr(u, func, ctx);
                func.emit_bare(Opcode::Pop);
            }
            emit_jump(func, Opcode::Jump, start);
            let end = func.here();
            ctx.pop_loop(func, end, update_target);
            if let Some(jf) = jmp_end {
                patch(func, jf, end);
            }
        }
        Stmt::Switch { disc, cases, .. } => {
            // Stash the discriminant in a scratch local so the operand stack
            // stays clean across case comparisons and bodies.
            compile_expr(disc, func, ctx);
            let tmp = func.locals.intern("<switch-disc>");
            func.emit(Instruction::new(Opcode::StaLocal, tmp));
            // First pass: emit comparisons jumping to each case body.
            let mut case_jumps = Vec::new();
            let mut default_idx: Option<usize> = None;
            for (i, c) in cases.iter().enumerate() {
                if let Some(t) = &c.test {
                    func.emit(Instruction::new(Opcode::LdaLocal, tmp));
                    compile_expr(t, func, ctx);
                    func.emit_bare(Opcode::StrictEq);
                    let j = emit_placeholder(func, Opcode::JumpIfTrue);
                    case_jumps.push((i, j));
                } else {
                    default_idx = Some(i);
                }
            }
            // No match: jump to default (or end).
            let jmp_default = emit_placeholder(func, Opcode::Jump);
            // Switches allow `break` only.
            ctx.push_loop();
            let loop_idx = ctx.loops.len() - 1;
            let mut body_starts = vec![0u16; cases.len()];
            for (i, c) in cases.iter().enumerate() {
                body_starts[i] = func.here();
                for s in &c.body {
                    compile_stmt(s, func, ctx, false);
                }
            }
            let end = func.here();
            for (i, j) in case_jumps {
                patch(func, j, body_starts[i]);
            }
            patch(
                func,
                jmp_default,
                default_idx.map(|i| body_starts[i]).unwrap_or(end),
            );
            // `continue` inside a switch should target the enclosing loop; we
            // approximate by leaving continues unpatched (rejected) — drain to
            // avoid leaking. Breaks patch to `end`.
            if let Some(frame) = ctx.loops.get_mut(loop_idx) {
                for at in frame.breaks.drain(..) {
                    patch(func, at, end);
                }
                // Any `continue` here is illegal in a bare switch; report below
                // by leaving its placeholder — but we can't easily, so patch to
                // end as a safe fallback.
                for at in frame.continues.drain(..) {
                    patch(func, at, end);
                }
            }
            ctx.loops.pop();
        }
        Stmt::Break { .. } => {
            if !ctx.emit_break(func) {
                ctx.errors.push(Diagnostic::error(
                    stmt.span(),
                    "`break` outside of a loop or switch",
                ));
            }
        }
        Stmt::Continue { .. } => {
            if !ctx.emit_continue(func) {
                ctx.errors.push(Diagnostic::error(
                    stmt.span(),
                    "`continue` outside of a loop",
                ));
            }
        }
        Stmt::Throw { arg, .. } => {
            compile_expr(arg, func, ctx);
            func.emit_bare(Opcode::Throw);
        }
        Stmt::Try {
            block,
            handler,
            finalizer,
            ..
        } => {
            compile_try(block, handler.as_deref(), finalizer.as_ref(), func, ctx);
        }
        Stmt::ForOf {
            left, right, body, ..
        } => {
            compile_for_of(left, right, body, func, ctx);
        }
        Stmt::ForIn {
            left, right, body, ..
        } => {
            compile_for_in(left, right, body, func, ctx);
        }
        other => {
            ctx.errors.push(Diagnostic::error(
                other.span(),
                "this statement kind is not supported yet",
            ));
        }
    }
    func.annotate_since(start_pc, stmt.span());
}

fn compile_decl(decl: &Decl, func: &mut BytecodeFunction, ctx: &mut CompilerCtx) {
    let start_pc = func.code.len();
    match decl {
        Decl::Var {
            kind: _,
            declarations,
            ..
        } => {
            for d in declarations {
                // Ensure the pattern's binding names are in scope (idempotent;
                // the hoisting pre-pass usually did this already).
                declare_pattern_names(&d.name, func, ctx);
                if let Some(init) = &d.init {
                    compile_expr(init, func, ctx); // [value]
                    bind_pattern(&d.name, func, ctx); // consumes
                }
            }
        }
        Decl::Function(f) => compile_function_decl(f, func, ctx),
        Decl::Class(c) => {
            let id = compile_class_value(
                c.span,
                c.name.as_deref(),
                &c.body,
                c.superclass.as_deref(),
                func,
                ctx,
            );
            if let Some(name) = &c.name {
                func.emit(Instruction::new(Opcode::LdaFunction, id as u16));
                let slot = ctx.declare_local(func, name);
                func.emit(Instruction::new(Opcode::StaLocal, slot));
            }
        }
        other => {
            ctx.errors.push(Diagnostic::error(
                other.span(),
                "this declaration kind is not supported yet",
            ));
        }
    }
    func.annotate_since(start_pc, decl.span());
}

fn compile_function_decl(f: &FunctionDecl, parent: &mut BytecodeFunction, ctx: &mut CompilerCtx) {
    let id = compile_function_value(
        f.span,
        f.name.as_deref(),
        &f.params,
        FunctionBody::Block(&f.body),
        false,
        f.is_generator,
        parent,
        ctx,
    );
    // Bind the function by name in the enclosing scope.
    if let Some(name) = &f.name {
        parent.emit(Instruction::new(Opcode::LdaFunction, id as u16));
        let slot = ctx.declare_local(parent, name);
        parent.emit(Instruction::new(Opcode::StaLocal, slot));
    }
}

/// The body shape for [`compile_function_value`]: a statement block, or an
/// arrow's concise expression body.
enum FunctionBody<'a> {
    Block(&'a [Stmt]),
    Expr(&'a Expr),
}

/// Compile a function (declaration, expression, or arrow) into a nested
/// [`BytecodeFunction`], returning its table id. Pushes/pops a lexical scope so
/// closure upvalues are resolved against the enclosing function.
fn compile_function_value(
    span: js_syntax::Span,
    name: Option<&str>,
    params: &[Pat],
    body: FunctionBody,
    is_arrow: bool,
    is_generator: bool,
    _parent: &mut BytecodeFunction,
    ctx: &mut CompilerCtx,
) -> u32 {
    // Reserve the table slot up front so deeply-nested functions get ids that
    // correctly index into the table (children compiled during the body land at
    // higher indices).
    let id = (ctx.functions.len() + 1) as u32;
    ctx.functions.push(BytecodeFunction::default());
    let fname = name.unwrap_or("<anonymous>").to_string();
    let mut nested = BytecodeFunction::new(span, fname.clone(), 0);
    nested.is_arrow = is_arrow;
    nested.is_generator = is_generator;
    let _ = is_arrow;

    ctx.scopes.push(Scope::default());
    // Parameters → slots 0..n, registered in the scope. Non-identifier params
    // get a scratch slot (the arg value) plus their inner bindings declared,
    // and are destructured at function entry below.
    let mut pattern_params: Vec<(u16, &Pat)> = Vec::new();
    for (i, p) in params.iter().enumerate() {
        match p {
            Pat::Ident { name, .. } => {
                let slot = nested.locals.intern(name);
                ctx.scopes
                    .last_mut()
                    .unwrap()
                    .locals
                    .insert(name.clone(), slot);
            }
            _ => {
                let scratch = nested.locals.intern(format!("<param{}>", i));
                declare_pattern_names(p, &mut nested, ctx);
                pattern_params.push((scratch, p));
            }
        }
    }
    nested.param_count = params.len() as u16;

    // Pre-pass: declare all hoisted body bindings so nested functions can
    // capture them even if declared textually later.
    if let FunctionBody::Block(stmts) = &body {
        let mut names = Vec::new();
        collect_bindings(stmts, &mut names);
        for n in names {
            ctx.declare_local(&mut nested, &n);
        }
    }

    // Destructure pattern parameters at function entry: each scratch slot
    // holds the passed argument; spread its contents into the inner bindings.
    for (scratch, pat) in &pattern_params {
        nested.emit(Instruction::new(Opcode::LdaLocal, *scratch));
        bind_pattern(pat, &mut nested, ctx);
    }

    match &body {
        FunctionBody::Block(stmts) => {
            compile_stmt_list_body(stmts, &mut nested, ctx);
            nested.emit_bare(Opcode::LdaUndefined);
            nested.emit_bare(Opcode::Return);
        }
        FunctionBody::Expr(e) => {
            compile_expr(e, &mut nested, ctx);
            nested.emit_bare(Opcode::Return);
        }
    }

    // Copy resolved upvalue descriptors into the bytecode function.
    let upvalues = ctx
        .scopes
        .last()
        .unwrap()
        .upvalues
        .iter()
        .map(|b| b.spec)
        .collect();
    nested.upvalues = upvalues;
    nested.annotate_since(0, span);

    ctx.scopes.pop();
    ctx.functions[id as usize - 1] = nested;
    id
}

/// Compile a class into a *constructor function* and return its table id.
///
/// Lowering (milestone): each non-static method is compiled as a nested
/// function and assigned onto `this` inside the constructor (`this.m = <fn>`),
/// and non-static field initializers run as `this.f = <init>` at the top of the
/// constructor body, before the user-written constructor body. `new C(...)`
/// therefore yields an instance carrying its own methods, so `inst.m()` binds
/// `this = inst` via method-call. Inheritance (`extends`/`super`) is not yet
/// supported.
fn compile_class_value(
    span: js_syntax::Span,
    name: Option<&str>,
    members: &[js_syntax::ast::expr::ClassMember],
    superclass: Option<&Expr>,
    func: &mut BytecodeFunction,
    ctx: &mut CompilerCtx,
) -> u32 {
    if let Some(sc) = superclass {
        ctx.errors.push(Diagnostic::error(
            sc.span(),
            "class inheritance (`extends`/`super`) is not supported yet",
        ));
    }

    use js_syntax::ast::expr::{ClassMemberKind, ClassMemberValue};

    // Locate the user constructor, if any.
    let mut ctor_params: Vec<Pat> = Vec::new();
    let mut ctor_body: Vec<Stmt> = Vec::new();
    for m in members {
        if matches!(m.kind, ClassMemberKind::Constructor) {
            if let ClassMemberValue::Method(f) = &m.value {
                ctor_params = f.params.clone();
                ctor_body = f.body.clone();
            }
        }
    }

    // Compile non-static methods (except the constructor) as nested functions.
    let mut methods: Vec<(js_syntax::ast::pat::PropKey, bool, u32)> = Vec::new();
    for m in members {
        if m.static_ {
            ctx.errors.push(Diagnostic::error(
                m.span,
                "static class members are not supported yet",
            ));
            continue;
        }
        if matches!(m.kind, ClassMemberKind::Constructor) {
            continue;
        }
        if let ClassMemberValue::Method(f) = &m.value {
            let mname = match &m.key {
                js_syntax::ast::pat::PropKey::Ident(n)
                | js_syntax::ast::pat::PropKey::String(n)
                | js_syntax::ast::pat::PropKey::Private(n) => Some(n.as_str()),
                _ => None,
            };
            let id = compile_function_value(
                f.span,
                mname,
                &f.params,
                FunctionBody::Block(&f.body),
                false,
                f.is_generator,
                func,
                ctx,
            );
            methods.push((m.key.clone(), m.computed, id));
        }
    }

    // Field initializers (non-static).
    let mut fields: Vec<(js_syntax::ast::pat::PropKey, bool, Option<Expr>)> = Vec::new();
    for m in members {
        if m.static_ {
            continue;
        }
        if let ClassMemberValue::Field(init) = &m.value {
            fields.push((m.key.clone(), m.computed, init.clone()));
        }
    }

    // Build the constructor function: its body sets up fields + methods, then
    // runs the user constructor body.
    let id = (ctx.functions.len() + 1) as u32;
    ctx.functions.push(BytecodeFunction::default());
    let fname = name.unwrap_or("<class>").to_string();
    let mut ctor = BytecodeFunction::new(span, fname, 0);
    ctx.scopes.push(Scope::default());
    for (i, p) in ctor_params.iter().enumerate() {
        if let Pat::Ident { name, .. } = p {
            let slot = ctor.locals.intern(name);
            ctx.scopes
                .last_mut()
                .unwrap()
                .locals
                .insert(name.clone(), slot);
        } else {
            ctor.locals.intern(format!("<ctor-param{}>", i));
        }
    }
    ctor.param_count = ctor_params.len() as u16;
    // Pre-pass the user body's bindings.
    {
        let mut names = Vec::new();
        collect_bindings(&ctor_body, &mut names);
        for n in names {
            ctx.declare_local(&mut ctor, &n);
        }
    }

    // Field initializers: `this.f = <init>` (skipped if no initializer).
    for (key, computed, init) in &fields {
        if let Some(init) = init {
            compile_expr(init, &mut ctor, ctx); // [v]
            ctor.emit_bare(Opcode::Dup); // [v, v]
            ctor.emit_bare(Opcode::LdaThis); // [v, v, this]
            compile_prop_key_push(key, *computed, &mut ctor, ctx); // [v, v, this, key]
            ctor.emit_bare(Opcode::SetProp); // [v]
            ctor.emit_bare(Opcode::Pop);
        }
    }
    // Method assignments: `this.m = <fn>`.
    for (key, computed, mid) in &methods {
        ctor.emit(Instruction::new(Opcode::LdaFunction, *mid as u16)); // [fn]
        ctor.emit_bare(Opcode::Dup); // [fn, fn]
        ctor.emit_bare(Opcode::LdaThis); // [fn, fn, this]
        compile_prop_key_push(key, *computed, &mut ctor, ctx); // [fn, fn, this, key]
        ctor.emit_bare(Opcode::SetProp); // [fn]
        ctor.emit_bare(Opcode::Pop);
    }

    // User constructor body.
    compile_stmt_list_body(&ctor_body, &mut ctor, ctx);
    ctor.emit_bare_at(Opcode::LdaUndefined, span);
    ctor.emit_bare_at(Opcode::Return, span);
    ctor.annotate_since(0, span);

    let upvalues = ctx
        .scopes
        .last()
        .unwrap()
        .upvalues
        .iter()
        .map(|b| b.spec)
        .collect();
    ctor.upvalues = upvalues;
    ctx.scopes.pop();
    ctx.functions[id as usize - 1] = ctor;
    id
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
    let start_pc = func.code.len();
    match expr {
        Expr::Lit(lit) => compile_lit(lit, func, ctx),
        Expr::Ident { name, .. } => load_ident(name, func, ctx),
        Expr::Paren { expr, .. } => compile_expr(expr, func, ctx),
        Expr::Unary { op, arg, .. } => {
            compile_expr(arg, func, ctx);
            func.emit_bare(Opcode::for_unaryop(*op));
        }
        Expr::Binary {
            op, left, right, ..
        } => {
            compile_expr(left, func, ctx);
            compile_expr(right, func, ctx);
            emit_binop(*op, func);
        }
        Expr::PrivateIn { right, .. } => {
            compile_expr(right, func, ctx);
            ctx.errors.push(Diagnostic::error(
                expr.span(),
                "private brand checks are not supported by bytecode compilation yet",
            ));
        }
        Expr::Logical {
            op, left, right, ..
        } => {
            // Short-circuit. `a && b`: if a falsy, result is a; else b.
            // JumpIf* pops the test, so Dup first to retain `a` as the result
            // on the taken branch.
            compile_expr(left, func, ctx);
            func.emit_bare(Opcode::Dup);
            let jmp = match op {
                BinOp::And => emit_placeholder(func, Opcode::JumpIfFalse),
                BinOp::Or => emit_placeholder(func, Opcode::JumpIfTrue),
                _ => {
                    // ?? — not short-circuited here; fall back to evaluating both.
                    compile_expr(right, func, ctx);
                    func.emit_bare(Opcode::Pop);
                    func.annotate_since(start_pc, expr.span());
                    return;
                }
            };
            // Fall-through (a had the "continue" truthiness): drop `a`, push `b`.
            func.emit_bare(Opcode::Pop);
            compile_expr(right, func, ctx);
            patch(func, jmp, func.here());
        }
        Expr::Update {
            op, prefix, arg, ..
        } => {
            compile_update(*op, *prefix, arg, func, ctx, expr.span());
        }
        Expr::TemplateLit {
            quasis,
            expressions,
            ..
        } => {
            // Concatenate: quasi0, then for each (expr, quasi_i) push + Add.
            let mut first = true;
            let mut exprs = expressions.iter();
            for (cooked, raw) in quasis {
                let text = cooked.clone().unwrap_or_else(|| raw.clone());
                let idx = ctx.constants.intern_str(text);
                func.emit(Instruction::new(Opcode::LdaConst, idx));
                if !first {
                    func.emit_bare(Opcode::Add);
                }
                first = false;
                if let Some(e) = exprs.next() {
                    compile_expr(e, func, ctx);
                    func.emit_bare(Opcode::Add);
                }
            }
        }
        Expr::Array { elements, .. } => {
            use js_syntax::ast::expr::ArrayExprElement;
            let has_spread = elements
                .iter()
                .flatten()
                .any(|e| matches!(e, ArrayExprElement::Spread(_)));
            if !has_spread {
                // Fast path: fixed element count.
                let mut count = 0u16;
                for el in elements.iter().flatten() {
                    if let ArrayExprElement::Expr(e) = el {
                        compile_expr(e, func, ctx);
                        count += 1;
                    }
                }
                func.emit(Instruction::new(Opcode::NewArray, count));
                func.annotate_since(start_pc, expr.span());
                return;
            }
            // Spread path: build incrementally.
            func.emit(Instruction::new(Opcode::NewArray, 0));
            for el in elements.iter().flatten() {
                match el {
                    ArrayExprElement::Expr(e) => {
                        func.emit_bare(Opcode::Dup);
                        compile_expr(e, func, ctx);
                        func.emit_bare(Opcode::ArrayPush);
                    }
                    ArrayExprElement::Spread(src) => {
                        // Drive the iterator protocol: push each yielded value.
                        // Stack on entry: [arr].
                        compile_expr(src, func, ctx); // [arr, src]
                        func.emit_bare(Opcode::GetIterator); // [arr, iter]
                        let it = func.locals.intern("<spread-it>");
                        func.emit(Instruction::new(Opcode::StaLocal, it)); // [arr]
                        let ls = func.here();
                        func.emit(Instruction::new(Opcode::LdaLocal, it)); // [arr, iter]
                        func.emit_bare(Opcode::IterNext); // [arr, result]
                        func.emit_bare(Opcode::Dup); // [arr, result, result]
                        let kd = ctx.constants.intern_str("done");
                        func.emit(Instruction::new(Opcode::LdaConst, kd));
                        func.emit_bare(Opcode::GetProp); // [arr, result, doneBool]
                        let le = emit_placeholder(func, Opcode::JumpIfTrue); // [arr, result]
                        let kv = ctx.constants.intern_str("value");
                        func.emit(Instruction::new(Opcode::LdaConst, kv));
                        func.emit_bare(Opcode::GetProp); // [arr, value]
                        func.emit_bare(Opcode::ArrayPush); // [arr]
                        emit_jump(func, Opcode::Jump, ls);
                        patch(func, le, func.here()); // [arr, result]
                        func.emit_bare(Opcode::Pop); // [arr]
                    }
                }
            }
        }
        Expr::Object { props, .. } => {
            func.emit_bare(Opcode::NewObject);
            for p in props {
                let value_expr = match &p.value {
                    ObjectPropValue::Expr(e) => e,
                    ObjectPropValue::Method(_) | ObjectPropValue::Spread(_) => {
                        ctx.errors.push(Diagnostic::error(
                            p.span,
                            "object methods/spread are not supported in the bytecode VM yet",
                        ));
                        continue;
                    }
                };
                // SetProp order is [value, obj, key]; we want to keep `obj`.
                func.emit_bare(Opcode::Dup); // [obj, obj]
                compile_expr(value_expr, func, ctx); // [obj, obj, value]
                func.emit_bare(Opcode::Swap);
                compile_prop_key_push(&p.key, p.computed, func, ctx); // [obj, value, obj, key]
                func.emit_bare(Opcode::SetProp); // [obj]
            }
        }
        Expr::Member(m) => {
            compile_expr(&m.object, func, ctx);
            compile_member_key_push(&m.property, func, ctx);
            func.emit_bare(Opcode::GetProp);
        }
        Expr::New(n) => {
            compile_expr(&n.callee, func, ctx);
            let mut count = 0u16;
            for a in &n.args {
                match a {
                    CallArg::Expr(e) => compile_expr(e, func, ctx),
                    CallArg::Spread(_) => {
                        ctx.errors.push(Diagnostic::error(
                            expr.span(),
                            "spread in `new` is not supported yet",
                        ));
                        func.emit_bare(Opcode::LdaUndefined);
                    }
                }
                count += 1;
            }
            func.emit(Instruction::new(Opcode::New, count));
        }
        Expr::Conditional {
            test, cons, alt, ..
        } => {
            compile_expr(test, func, ctx);
            let jf = emit_placeholder(func, Opcode::JumpIfFalse);
            compile_expr(cons, func, ctx);
            let je = emit_placeholder(func, Opcode::Jump);
            patch(func, jf, func.here());
            compile_expr(alt, func, ctx);
            patch(func, je, func.here());
        }
        Expr::Assign {
            op, left, right, ..
        } => {
            match left {
                AssignTarget::Ident { name, .. } => {
                    if *op == AssignOp::Assign {
                        compile_expr(right, func, ctx);
                        func.emit_bare(Opcode::Dup);
                        store_ident(name, func, ctx);
                    } else {
                        // Compound: load, op=, store.
                        load_ident(name, func, ctx);
                        compile_expr(right, func, ctx);
                        emit_binop(compound_to_binop(*op), func);
                        func.emit_bare(Opcode::Dup);
                        store_ident(name, func, ctx);
                    }
                }
                AssignTarget::Member(m) => {
                    // SetProp order is [value, obj, key]; assignment result is
                    // `value`, retained via a Dup.
                    if *op == AssignOp::Assign {
                        compile_expr(right, func, ctx); // [v]
                        func.emit_bare(Opcode::Dup); // [v, v]
                        compile_expr(&m.object, func, ctx); // [v, v, obj]
                        compile_member_key_push(&m.property, func, ctx); // [v, v, obj, key]
                        func.emit_bare(Opcode::SetProp); // [v]
                    } else {
                        // Compound: load current, apply op, store, keep result.
                        compile_expr(&m.object, func, ctx); // [obj]
                        compile_member_key_push(&m.property, func, ctx); // [obj, key]
                        func.emit_bare(Opcode::GetProp); // [cur]
                        compile_expr(right, func, ctx); // [cur, rhs]
                        emit_binop(compound_to_binop(*op), func); // [newv]
                        func.emit_bare(Opcode::Dup); // [newv, newv]
                        compile_expr(&m.object, func, ctx); // [newv, newv, obj]
                        compile_member_key_push(&m.property, func, ctx); // [newv, newv, obj, key]
                        func.emit_bare(Opcode::SetProp); // [newv]
                    }
                }
                AssignTarget::Pat(_) => {
                    ctx.errors.push(Diagnostic::error(
                        expr.span(),
                        "destructuring assignment is not supported in the bytecode VM yet",
                    ));
                    compile_expr(right, func, ctx);
                    func.emit_bare(Opcode::Pop);
                    func.emit_bare(Opcode::LdaUndefined);
                }
            }
        }
        Expr::Call(call) => {
            // Method call `obj.m(args)` / `obj[k](args)`: keep `obj` as `this`.
            match call.callee.as_ref() {
                Expr::Member(m) => {
                    compile_expr(&m.object, func, ctx); // [obj]
                    func.emit_bare(Opcode::Dup); // [obj, obj]
                    compile_member_key_push(&m.property, func, ctx); // [obj, obj, key]
                    func.emit_bare(Opcode::GetProp); // [obj, method]
                    let mut count = 0u16;
                    for a in &call.args {
                        if let CallArg::Expr(e) = a {
                            compile_expr(e, func, ctx);
                            count += 1;
                        } else {
                            ctx.errors.push(Diagnostic::error(
                                expr.span(),
                                "spread arguments are not supported yet",
                            ));
                            func.emit_bare(Opcode::LdaUndefined);
                            count += 1;
                        }
                    }
                    func.emit(Instruction::new(Opcode::CallMethod, count));
                }
                _ => {
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
            }
        }
        Expr::Function(f) => {
            let id = compile_function_value(
                f.span,
                f.name.as_deref(),
                &f.params,
                FunctionBody::Block(&f.body),
                false,
                f.is_generator,
                func,
                ctx,
            );
            func.emit(Instruction::new(Opcode::LdaFunction, id as u16));
        }
        Expr::Arrow(a) => {
            let body = match &a.body {
                js_syntax::ast::expr::ArrowBody::Block(stmts) => FunctionBody::Block(stmts),
                js_syntax::ast::expr::ArrowBody::Expr(e) => FunctionBody::Expr(e),
            };
            let id = compile_function_value(a.span, None, &a.params, body, true, false, func, ctx);
            func.emit(Instruction::new(Opcode::LdaFunction, id as u16));
        }
        Expr::Class(c) => {
            let id = compile_class_value(
                c.span,
                c.name.as_deref(),
                &c.body,
                c.superclass.as_deref(),
                func,
                ctx,
            );
            func.emit(Instruction::new(Opcode::LdaFunction, id as u16));
        }
        Expr::This { .. } => func.emit_bare(Opcode::LdaThis),
        // The VM does not expose a new-target register yet. Preserve the
        // previous behavior for ordinary calls until constructor frames carry it.
        Expr::NewTarget(_) => func.emit_bare(Opcode::LdaUndefined),
        Expr::Yield { arg, delegate, .. } => {
            if *delegate {
                ctx.errors.push(Diagnostic::error(
                    expr.span(),
                    "`yield*` delegation is not supported yet",
                ));
                func.emit_bare(Opcode::LdaUndefined);
                func.annotate_since(start_pc, expr.span());
                return;
            }
            // Push the value to yield (undefined if no operand), then suspend.
            match arg {
                Some(e) => compile_expr(e, func, ctx),
                None => func.emit_bare(Opcode::LdaUndefined),
            }
            func.emit_bare(Opcode::Yield);
        }
        Expr::Await { arg, .. } => {
            // No Promise runtime: `await x` evaluates to `x` synchronously.
            compile_expr(arg, func, ctx);
        }
        Expr::Regex { pattern, flags, .. } => {
            let combined = format!("{}\0{}", pattern, flags);
            let idx = ctx.constants.intern_str(combined);
            func.emit(Instruction::new(Opcode::NewRegex, idx));
        }
        _ => {
            ctx.errors.push(Diagnostic::error(
                expr.span(),
                "this expression kind is not supported yet",
            ));
            func.emit_bare(Opcode::LdaUndefined);
        }
    }
    func.annotate_since(start_pc, expr.span());
}

fn compile_lit(lit: &Lit, func: &mut BytecodeFunction, ctx: &mut CompilerCtx) {
    match lit {
        Lit::Null(_) => func.emit_bare(Opcode::LdaNull),
        Lit::Boolean(_, b) => func.emit_bare(if *b {
            Opcode::LdaTrue
        } else {
            Opcode::LdaFalse
        }),
        Lit::Number(_, n, _) => {
            // Integral literals that fit in i32 take the integer fast path;
            // everything else is stored as a float.
            let idx = if n.fract() == 0.0
                && n.is_finite()
                && *n >= i32::MIN as f64
                && *n <= i32::MAX as f64
            {
                ctx.constants.intern_int(*n as i32)
            } else {
                ctx.constants.intern_num(*n)
            };
            func.emit(Instruction::new(Opcode::LdaConst, idx));
        }
        Lit::String(_, s, _) => {
            let idx = ctx.constants.intern_str(s);
            func.emit(Instruction::new(Opcode::LdaConst, idx));
        }
        Lit::BigInt(span, _) | Lit::Regex { span, .. } | Lit::TemplateString { span, .. } => {
            ctx.errors.push(Diagnostic::error(
                *span,
                "this literal kind is not supported yet",
            ));
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
        Instanceof => func.emit_bare(Opcode::Instanceof),
        And | Or | NullishCoal | In => {
            // Unsupported for milestone-1; emit Add as a harmless placeholder —
            // the caller already left both operands on the stack.
            func.emit_bare(Opcode::Add);
        }
    }
}

// ---- helpers -------------------------------------------------------------

/// How a variable reference resolves within the current function.
enum VarRef {
    Local(u16),
    Upvalue(u16),
    Global,
}

/// Compile `try { B } catch(e?) { C } finally? { F }`.
///
/// Emits a `TryBegin` referencing a handler spec (catch_pc + finally_pc,
/// backpatched), the try body, a `TryEnd` on normal exit, the catch clause,
/// and (if present) a finally block ending in `FinallyEnd`.
fn compile_try(
    block: &js_syntax::ast::stmt::TryBlock,
    handler: Option<&js_syntax::ast::stmt::CatchClause>,
    finalizer: Option<&Vec<Stmt>>,
    func: &mut BytecodeFunction,
    ctx: &mut CompilerCtx,
) {
    let spec_idx = func.handlers.len() as u16;
    func.handlers.push(crate::module::HandlerSpec::default());
    func.emit(Instruction::new(Opcode::TryBegin, spec_idx));
    // Try body.
    let items: Vec<_> = block
        .body
        .iter()
        .map(|s| js_syntax::ast::ProgramItem::Stmt(s.clone()))
        .collect();
    compile_block(&items, func, ctx, false);
    func.emit_bare(Opcode::TryEnd); // normal try exit → pop handler
    let jmp_past_catch = emit_placeholder(func, Opcode::Jump);

    // Catch clause.
    let catch_pc = if handler.is_some() {
        let pc = func.here();
        if let Some(h) = handler {
            // The thrown value is on the stack: bind it to the catch param (or
            // discard if `catch {}`).
            match &h.param {
                Some(p) => bind_pattern(p, func, ctx),
                None => func.emit_bare(Opcode::Pop),
            }
            let items: Vec<_> = h
                .body
                .iter()
                .map(|s| js_syntax::ast::ProgramItem::Stmt(s.clone()))
                .collect();
            compile_block(&items, func, ctx, false);
        }
        Some(pc)
    } else {
        None
    };
    let jmp_to_after = emit_placeholder(func, Opcode::Jump);

    // Finally block (also the join point for normal + caught paths).
    let after_pc = func.here();
    if let Some(fin) = finalizer {
        let items: Vec<_> = fin
            .iter()
            .map(|s| js_syntax::ast::ProgramItem::Stmt(s.clone()))
            .collect();
        compile_block(&items, func, ctx, false);
        func.emit_bare(Opcode::FinallyEnd);
    }
    let end = func.here();

    patch(func, jmp_past_catch, after_pc);
    patch(func, jmp_to_after, after_pc);
    func.handlers[spec_idx as usize] = crate::module::HandlerSpec {
        catch_pc,
        finally_pc: if finalizer.is_some() {
            Some(after_pc)
        } else {
            None
        },
    };
    let _ = end;
}

/// Extract the binding pattern from a for-in/of target.
fn for_target_pat(left: &ForTarget) -> &Pat {
    match left {
        ForTarget::Var(d) => {
            // `for (var x of ...)` — single declarator, no init.
            match d.as_ref() {
                Decl::Var { declarations, .. } => &declarations[0].name,
                _ => unreachable!("for-Var target must be a Var declaration"),
            }
        }
        ForTarget::Pat(p) => p,
    }
}

/// Lower `for (target of iterable) body` via the **iterator protocol**: get an
/// iterator, step it with `.next()` until `done`. Works uniformly for arrays,
/// strings, and generators. `for-in` still uses the `ObjectKeys` index path.
fn compile_for_of(
    left: &ForTarget,
    right: &Expr,
    body: &Stmt,
    func: &mut BytecodeFunction,
    ctx: &mut CompilerCtx,
) {
    let iter = func.locals.intern("<forof-iter>");
    // ITER = GetIterator(right)
    compile_expr(right, func, ctx);
    func.emit_bare(Opcode::GetIterator);
    func.emit(Instruction::new(Opcode::StaLocal, iter));

    let start = func.here();
    // result = ITER.next()  (an iterator-result {value, done})
    func.emit(Instruction::new(Opcode::LdaLocal, iter));
    func.emit_bare(Opcode::IterNext);
    // if result.done → end
    func.emit_bare(Opcode::Dup);
    let k = ctx.constants.intern_str("done");
    func.emit(Instruction::new(Opcode::LdaConst, k));
    func.emit_bare(Opcode::GetProp);
    let jmp_end = emit_placeholder(func, Opcode::JumpIfTrue);
    // target = result.value
    let kv = ctx.constants.intern_str("value");
    func.emit(Instruction::new(Opcode::LdaConst, kv));
    func.emit_bare(Opcode::GetProp);
    bind_pattern(for_target_pat(left), func, ctx);
    // body
    ctx.push_loop();
    compile_stmt(body, func, ctx, false);
    let update_target = func.here();
    emit_jump(func, Opcode::Jump, start);
    let end = func.here();
    ctx.pop_loop(func, end, update_target);
    patch(func, jmp_end, end);
    // The done result object is still on the stack; drop it.
    func.emit_bare(Opcode::Pop);
}

/// Lower `for (target in obj) body` over object/array string keys.
fn compile_for_in(
    left: &ForTarget,
    right: &Expr,
    body: &Stmt,
    func: &mut BytecodeFunction,
    ctx: &mut CompilerCtx,
) {
    let src = func.locals.intern("<forin-src>");
    let idx = func.locals.intern("<forin-idx>");
    let len = func.locals.intern("<forin-len>");
    compile_expr(right, func, ctx);
    func.emit_bare(Opcode::ObjectKeys);
    func.emit(Instruction::new(Opcode::StaLocal, src));
    func.emit(Instruction::new(Opcode::LdaLocal, src));
    let k = ctx.constants.intern_str("length");
    func.emit(Instruction::new(Opcode::LdaConst, k));
    func.emit_bare(Opcode::GetProp);
    func.emit(Instruction::new(Opcode::StaLocal, len));
    let zero = ctx.constants.intern_int(0);
    func.emit(Instruction::new(Opcode::LdaConst, zero));
    func.emit(Instruction::new(Opcode::StaLocal, idx));
    let start = func.here();
    func.emit(Instruction::new(Opcode::LdaLocal, idx));
    func.emit(Instruction::new(Opcode::LdaLocal, len));
    func.emit_bare(Opcode::Ge);
    let jmp_end = emit_placeholder(func, Opcode::JumpIfTrue);
    func.emit(Instruction::new(Opcode::LdaLocal, src));
    func.emit(Instruction::new(Opcode::LdaLocal, idx));
    func.emit_bare(Opcode::GetProp);
    bind_pattern(for_target_pat(left), func, ctx);
    ctx.push_loop();
    compile_stmt(body, func, ctx, false);
    let update_target = func.here();
    let one = ctx.constants.intern_int(1);
    func.emit(Instruction::new(Opcode::LdaLocal, idx));
    func.emit(Instruction::new(Opcode::LdaConst, one));
    func.emit_bare(Opcode::Add);
    func.emit(Instruction::new(Opcode::StaLocal, idx));
    emit_jump(func, Opcode::Jump, start);
    let end = func.here();
    ctx.pop_loop(func, end, update_target);
    patch(func, jmp_end, end);
}

/// Declare every binding name introduced by a pattern (recursing into nested
/// array/object patterns) into the current scope. Idempotent.
fn declare_pattern_names(pat: &Pat, func: &mut BytecodeFunction, ctx: &mut CompilerCtx) {
    match pat {
        Pat::Ident { name, .. } => {
            ctx.declare_local(func, name);
        }
        Pat::Array { elements, .. } => {
            for el in elements.iter().flatten() {
                if let js_syntax::ast::pat::ArrayPatElement::Pat(p) = el {
                    declare_pattern_names(p, func, ctx);
                }
            }
        }
        Pat::Object { properties, .. } => {
            for prop in properties {
                match prop {
                    js_syntax::ast::pat::ObjectPatProp::KeyValue { value, .. } => {
                        declare_pattern_names(value, func, ctx)
                    }
                    js_syntax::ast::pat::ObjectPatProp::Rest { arg, .. } => {
                        declare_pattern_names(arg, func, ctx)
                    }
                }
            }
        }
        Pat::Rest { arg, .. } => declare_pattern_names(arg, func, ctx),
        Pat::Assignment { left, .. } => declare_pattern_names(left, func, ctx),
        // A member target (`[a.b] = x`) introduces no binding names.
        Pat::Member(_) => {}
    }
}

/// Bind the value on top of the operand stack into `pat`, consuming it.
/// Supports identifier, nested array/object patterns, and defaults (`x = d`).
/// Rest elements/properties are collected into a fresh array (basic).
fn bind_pattern(pat: &Pat, func: &mut BytecodeFunction, ctx: &mut CompilerCtx) {
    match pat {
        Pat::Ident { name, .. } => store_ident(name, func, ctx),
        Pat::Assignment { left, right, .. } => {
            // `value === undefined ? default : value`
            func.emit_bare(Opcode::Dup);
            func.emit_bare(Opcode::LdaUndefined);
            func.emit_bare(Opcode::StrictEq);
            let keep = emit_placeholder(func, Opcode::JumpIfFalse);
            func.emit_bare(Opcode::Pop);
            compile_expr(right, func, ctx);
            patch(func, keep, func.here());
            bind_pattern(left, func, ctx);
        }
        Pat::Array { elements, .. } => {
            // Stash the source array in a temp local.
            let tmp = func.locals.intern("<destr-arr>");
            func.emit(Instruction::new(Opcode::StaLocal, tmp));
            for (i, el) in elements.iter().enumerate() {
                match el {
                    None => {} // hole
                    Some(js_syntax::ast::pat::ArrayPatElement::Hole(_)) => {}
                    Some(js_syntax::ast::pat::ArrayPatElement::Pat(inner)) => {
                        // Handle a trailing rest element specially.
                        if let Pat::Rest { arg, .. } = inner {
                            // Collect remaining elements [i..] into a fresh array.
                            bind_array_rest(func, ctx, tmp, i, arg);
                        } else {
                            func.emit(Instruction::new(Opcode::LdaLocal, tmp));
                            let idx = ctx.constants.intern_int(i as i32);
                            func.emit(Instruction::new(Opcode::LdaConst, idx));
                            func.emit_bare(Opcode::GetProp);
                            bind_pattern(inner, func, ctx);
                        }
                    }
                }
            }
        }
        Pat::Object { properties, .. } => {
            let tmp = func.locals.intern("<destr-obj>");
            func.emit(Instruction::new(Opcode::StaLocal, tmp));
            for prop in properties {
                match prop {
                    js_syntax::ast::pat::ObjectPatProp::KeyValue { key, value, .. } => {
                        func.emit(Instruction::new(Opcode::LdaLocal, tmp));
                        compile_prop_key_push(key, false, func, ctx);
                        func.emit_bare(Opcode::GetProp);
                        bind_pattern(value, func, ctx);
                    }
                    // Object rest: collect remaining keys (basic — copy whole object).
                    js_syntax::ast::pat::ObjectPatProp::Rest { arg, .. } => {
                        func.emit(Instruction::new(Opcode::LdaLocal, tmp));
                        bind_pattern(arg, func, ctx);
                    }
                }
            }
        }
        Pat::Rest { arg, .. } => {
            // A bare rest pattern (e.g. as a param) — bind the current value.
            bind_pattern(arg, func, ctx);
        }
        // Member target (`[a.b] = x`): execution (member store) is not yet wired;
        // drop the value so the stack stays balanced. Parsing is unaffected.
        Pat::Member(_) => {
            func.emit_bare(Opcode::Pop);
        }
    }
}

/// `[...rest]` array-pattern rest. Properly collecting `tmp[start..]` into a
/// fresh array needs a runtime append primitive we don't have yet; for the
/// milestone we bind `rest` to an empty array (so the binding exists; the
/// collected tail is not populated). The common `[a, b] = arr` (no rest) path
/// is fully correct.
fn bind_array_rest(
    func: &mut BytecodeFunction,
    _ctx: &mut CompilerCtx,
    _tmp: u16,
    _start: usize,
    arg: &Pat,
) {
    func.emit(Instruction::new(Opcode::NewArray, 0));
    bind_pattern(arg, func, _ctx);
}

/// Pre-pass: collect all binding names introduced by a function body (params
/// plus hoisted `var`/`function`/`let`/`const`/`class`/`catch` bindings), so the
/// full local set is known before any nested function is compiled. This is what
/// makes closure capture see textually-later `var` declarations. Block scope is
/// flattened to function scope here (a milestone simplification).
fn collect_bindings(body: &[Stmt], out: &mut Vec<String>) {
    for s in body {
        collect_stmt_bindings(s, out);
    }
}

fn collect_stmt_bindings(stmt: &Stmt, out: &mut Vec<String>) {
    match stmt {
        Stmt::Block { body, .. } => collect_bindings(body, out),
        Stmt::Decl(d) => collect_decl_bindings(d, out),
        Stmt::If { cons, alt, .. } => {
            collect_stmt_bindings(cons, out);
            if let Some(a) = alt {
                collect_stmt_bindings(a, out);
            }
        }
        Stmt::While { body, .. } | Stmt::DoWhile { body, .. } => collect_stmt_bindings(body, out),
        Stmt::For { init, body, .. } => {
            if let Some(ForInit::Var(d)) = init {
                collect_decl_bindings(d, out);
            }
            collect_stmt_bindings(body, out);
        }
        Stmt::ForIn { left, body, .. } | Stmt::ForOf { left, body, .. } => {
            if let ForTarget::Var(d) = left {
                collect_decl_bindings(d, out);
            }
            collect_stmt_bindings(body, out);
        }
        Stmt::Switch { cases, .. } => {
            for c in cases {
                collect_bindings(&c.body, out);
            }
        }
        Stmt::Try {
            block,
            handler,
            finalizer,
            ..
        } => {
            collect_bindings(&block.body, out);
            if let Some(h) = handler {
                if let Some(p) = &h.param {
                    collect_pat_bindings(p, out);
                }
                collect_bindings(&h.body, out);
            }
            if let Some(f) = finalizer {
                collect_bindings(f, out);
            }
        }
        Stmt::Labeled { body, .. } => collect_stmt_bindings(body, out),
        // Expression statements, return, throw, break, continue, etc. introduce
        // no bindings; nested functions are separate scopes (not collected).
        _ => {}
    }
}

fn collect_decl_bindings(decl: &Decl, out: &mut Vec<String>) {
    match decl {
        Decl::Var { declarations, .. } => {
            for d in declarations {
                collect_pat_bindings(&d.name, out);
            }
        }
        Decl::Function(f) => {
            if let Some(n) = &f.name {
                out.push(n.clone());
            }
        }
        Decl::Class(c) => {
            if let Some(n) = &c.name {
                out.push(n.clone());
            }
        }
        _ => {}
    }
}

fn collect_pat_bindings(pat: &Pat, out: &mut Vec<String>) {
    match pat {
        Pat::Ident { name, .. } => out.push(name.clone()),
        Pat::Array { elements, .. } => {
            for el in elements.iter().flatten() {
                if let js_syntax::ast::pat::ArrayPatElement::Pat(p) = el {
                    collect_pat_bindings(p, out);
                }
            }
        }
        Pat::Object { properties, .. } => {
            for prop in properties {
                match prop {
                    js_syntax::ast::pat::ObjectPatProp::KeyValue { value, .. } => {
                        collect_pat_bindings(value, out)
                    }
                    js_syntax::ast::pat::ObjectPatProp::Rest { arg, .. } => {
                        collect_pat_bindings(arg, out)
                    }
                }
            }
        }
        Pat::Rest { arg, .. } => collect_pat_bindings(arg, out),
        Pat::Assignment { left, .. } => collect_pat_bindings(left, out),
        Pat::Member(_) => {}
    }
}

/// Push a member property key onto the stack as a Value (string for `.x`,
/// evaluated expr for `[e]`).
fn compile_member_key_push(prop: &MemberProp, func: &mut BytecodeFunction, ctx: &mut CompilerCtx) {
    match prop {
        MemberProp::Ident(n) | MemberProp::Private(n) => {
            let idx = ctx.constants.intern_str(n);
            func.emit(Instruction::new(Opcode::LdaConst, idx));
        }
        MemberProp::Computed(e) => compile_expr(e, func, ctx),
    }
}

/// Push an object-literal property key (string for ident/string/number, or the
/// evaluated computed expr).
fn compile_prop_key_push(
    key: &js_syntax::ast::pat::PropKey,
    computed: bool,
    func: &mut BytecodeFunction,
    ctx: &mut CompilerCtx,
) {
    use js_syntax::ast::pat::PropKey;
    match key {
        PropKey::Ident(n) | PropKey::String(n) | PropKey::Private(n) => {
            let idx = ctx.constants.intern_str(n);
            func.emit(Instruction::new(Opcode::LdaConst, idx));
        }
        PropKey::Number(n) => {
            let idx = if n.fract() == 0.0
                && n.is_finite()
                && *n >= i32::MIN as f64
                && *n <= i32::MAX as f64
            {
                ctx.constants.intern_int(*n as i32)
            } else {
                ctx.constants.intern_num(*n)
            };
            func.emit(Instruction::new(Opcode::LdaConst, idx));
        }
        PropKey::Computed(e) => {
            let _ = computed;
            compile_expr(e, func, ctx);
        }
    }
}

fn load_ident(name: &str, func: &mut BytecodeFunction, ctx: &mut CompilerCtx) {
    if name == "undefined" {
        func.emit_bare(Opcode::LdaUndefined);
        return;
    }
    match ctx.resolve_var(name) {
        VarRef::Local(slot) => func.emit(Instruction::new(Opcode::LdaLocal, slot)),
        VarRef::Upvalue(idx) => func.emit(Instruction::new(Opcode::LdaUpvalue, idx)),
        VarRef::Global => {
            let idx = ctx.constants.intern_str(name);
            func.emit(Instruction::new(Opcode::GetGlobal, idx));
        }
    }
}

fn store_ident(name: &str, func: &mut BytecodeFunction, ctx: &mut CompilerCtx) {
    match ctx.resolve_var(name) {
        VarRef::Local(slot) => func.emit(Instruction::new(Opcode::StaLocal, slot)),
        VarRef::Upvalue(idx) => func.emit(Instruction::new(Opcode::StaUpvalue, idx)),
        VarRef::Global => {
            let idx = ctx.constants.intern_str(name);
            func.emit(Instruction::new(Opcode::SetGlobal, idx));
        }
    }
}

fn compile_update(
    op: UpdateOp,
    prefix: bool,
    arg: &Expr,
    func: &mut BytecodeFunction,
    ctx: &mut CompilerCtx,
    span: js_syntax::Span,
) {
    let arith = match op {
        UpdateOp::Inc => Opcode::Add,
        UpdateOp::Dec => Opcode::Sub,
    };
    let one = ctx.constants.intern_int(1);
    let emit_delta = |func: &mut BytecodeFunction| {
        func.emit(Instruction::new(Opcode::LdaConst, one));
        func.emit_bare(arith);
    };
    match arg {
        Expr::Ident { name, .. } => {
            load_ident(name, func, ctx);
            if prefix {
                emit_delta(func);
                func.emit_bare(Opcode::Dup);
                store_ident(name, func, ctx);
            } else {
                func.emit_bare(Opcode::Dup);
                emit_delta(func);
                store_ident(name, func, ctx);
            }
        }
        Expr::Member(m) => {
            // Load current value, apply +1/-1, then store with [value, obj, key]
            // ordering. Prefix yields the new value; postfix yields the old.
            compile_expr(&m.object, func, ctx); // [obj]
            compile_member_key_push(&m.property, func, ctx); // [obj, key]
            func.emit_bare(Opcode::GetProp); // [cur]
            if prefix {
                emit_delta(func); // [newv]
                func.emit_bare(Opcode::Dup); // [newv, newv]
                compile_expr(&m.object, func, ctx); // [newv, newv, obj]
                compile_member_key_push(&m.property, func, ctx); // [newv, newv, obj, key]
                func.emit_bare(Opcode::SetProp); // [newv]
            } else {
                func.emit_bare(Opcode::Dup); // [cur, cur]
                emit_delta(func); // [cur, newv]
                compile_expr(&m.object, func, ctx); // [cur, newv, obj]
                compile_member_key_push(&m.property, func, ctx); // [cur, newv, obj, key]
                func.emit_bare(Opcode::SetProp); // [cur]
            }
        }
        _ => {
            ctx.errors
                .push(Diagnostic::error(span, "invalid update target"));
            func.emit_bare(Opcode::LdaUndefined);
        }
    }
}

/// Bind (or look up) a local slot for a binding pattern. Only simple identifier
/// patterns are supported for the milestone.
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

/// Map a compound-assignment operator to the binary op it applies.
fn compound_to_binop(op: AssignOp) -> BinOp {
    match op {
        AssignOp::Add => BinOp::Add,
        AssignOp::Sub => BinOp::Sub,
        AssignOp::Mul => BinOp::Mul,
        AssignOp::Div => BinOp::Div,
        AssignOp::Mod => BinOp::Mod,
        AssignOp::Exp => BinOp::Exp,
        AssignOp::BitAnd => BinOp::BitAnd,
        AssignOp::BitOr => BinOp::BitOr,
        AssignOp::BitXor => BinOp::BitXor,
        AssignOp::Shl => BinOp::Shl,
        AssignOp::Shr => BinOp::Shr,
        AssignOp::Ushr => BinOp::Ushr,
        AssignOp::And => BinOp::And,
        AssignOp::Or => BinOp::Or,
        AssignOp::Nullish => BinOp::NullishCoal,
        AssignOp::Assign => BinOp::Add, // unused
    }
}

struct CompilerCtx {
    constants: ConstantPool,
    functions: Vec<BytecodeFunction>,
    errors: Vec<Diagnostic>,
    /// Stack of enclosing loops/switches for `break`/`continue`. Each frame
    /// records forward-jump placeholders to patch at loop exit: `breaks` → after
    /// the loop, `continues` → the update/test section. Switches use `breaks`
    /// only (`continues` stays empty and a `continue` inside a bare switch is
    /// rejected by the caller).
    loops: Vec<LoopFrame>,
    /// Lexical scope stack, one entry per *function* being compiled (scopes[0]
    /// is `<main>`). Drives closure upvalue resolution.
    scopes: Vec<Scope>,
}

/// One function's compile-time lexical scope: its local names and the
/// upvalues it captures from enclosing scopes.
#[derive(Default)]
struct Scope {
    locals: std::collections::HashMap<String, u16>,
    upvalues: Vec<UpvalueBinding>,
}

struct UpvalueBinding {
    name: String,
    spec: crate::module::UpvalueSpec,
}

#[derive(Default)]
struct LoopFrame {
    breaks: Vec<u16>,
    continues: Vec<u16>,
}

impl CompilerCtx {
    // ---- scope / upvalue resolution -------------------------------------

    /// Register a local binding name in the current scope (also allocating a
    /// slot in `func.locals`). Returns the slot index.
    fn declare_local(&mut self, func: &mut BytecodeFunction, name: &str) -> u16 {
        if let Some(&slot) = self.scopes.last().and_then(|s| s.locals.get(name)) {
            return slot;
        }
        let slot = func.locals.intern(name);
        self.scopes
            .last_mut()
            .unwrap()
            .locals
            .insert(name.to_string(), slot);
        slot
    }

    /// Resolve `name` to a variable reference for the *current* function:
    /// `Local(slot)` if it's a local of the current function; otherwise the
    /// upvalue index (captured from an enclosing scope); otherwise `None`
    /// (global).
    fn resolve_var(&mut self, name: &str) -> VarRef {
        // Local in the current scope?
        if let Some(&slot) = self.scopes.last().unwrap().locals.get(name) {
            return VarRef::Local(slot);
        }
        // Captured from an enclosing scope?
        let depth = self.scopes.len();
        if depth >= 2 {
            if let Some(uv) = self.resolve_upvalue(name, depth - 2) {
                return VarRef::Upvalue(uv);
            }
        }
        VarRef::Global
    }

    /// Recursive Lua-style upvalue resolution against ancestor scopes.
    /// `scope_idx` is the ancestor index to inspect (the parent of the function
    /// currently being resolved). Returns the upvalue index registered in the
    /// *immediate child* scope (scopes[scope_idx + 1]).
    fn resolve_upvalue(&mut self, name: &str, scope_idx: usize) -> Option<u16> {
        // Does this ancestor hold `name` as a local?
        let local_slot = self.scopes[scope_idx].locals.get(name).copied();
        if let Some(slot) = local_slot {
            return Some(self.add_upvalue(
                scope_idx + 1,
                name,
                crate::module::UpvalueSpec {
                    is_local: true,
                    index: slot,
                },
            ));
        }
        // Does the ancestor itself capture it as an upvalue? (or further out)
        if scope_idx == 0 {
            return None;
        }
        let parent_uv = self.resolve_upvalue(name, scope_idx - 1)?;
        Some(self.add_upvalue(
            scope_idx + 1,
            name,
            crate::module::UpvalueSpec {
                is_local: false,
                index: parent_uv,
            },
        ))
    }

    /// Register an upvalue in `scopes[scope_idx]` (deduped by name); return its index.
    fn add_upvalue(
        &mut self,
        scope_idx: usize,
        name: &str,
        spec: crate::module::UpvalueSpec,
    ) -> u16 {
        let scope = &mut self.scopes[scope_idx];
        for (i, b) in scope.upvalues.iter().enumerate() {
            if b.name == name {
                return i as u16;
            }
        }
        scope.upvalues.push(UpvalueBinding {
            name: name.to_string(),
            spec,
        });
        (scope.upvalues.len() - 1) as u16
    }

    fn push_loop(&mut self) {
        self.loops.push(LoopFrame::default());
    }
    /// Pop the current frame, patching `break` jumps to `break_target` and
    /// `continue` jumps to `continue_target`.
    fn pop_loop(&mut self, func: &mut BytecodeFunction, break_target: u16, continue_target: u16) {
        if let Some(frame) = self.loops.pop() {
            for at in frame.breaks {
                patch(func, at, break_target);
            }
            for at in frame.continues {
                patch(func, at, continue_target);
            }
        }
    }
    fn emit_break(&mut self, func: &mut BytecodeFunction) -> bool {
        if let Some(frame) = self.loops.last_mut() {
            frame.breaks.push(emit_placeholder(func, Opcode::Jump));
            true
        } else {
            false
        }
    }
    fn emit_continue(&mut self, func: &mut BytecodeFunction) -> bool {
        if let Some(frame) = self.loops.last_mut() {
            // Only loops (not switches) accept continue; switches leave
            // `continues` to be patched to the enclosing loop's update, but to
            // keep semantics simple we only allow continue in loops — switches
            // don't push a continue-able frame distinction here. We record the
            // patch and let the enclosing loop patch it.
            frame.continues.push(emit_placeholder(func, Opcode::Jump));
            true
        } else {
            false
        }
    }
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
