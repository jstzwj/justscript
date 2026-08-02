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
use js_syntax::ast::expr::{AssignTarget, CallArg, Expr, MemberExpr, MemberProp, ObjectPropValue};
use js_syntax::ast::lit::Lit;
use js_syntax::ast::op::{BinOp, UpdateOp};
use js_syntax::ast::pat::Pat;
use js_syntax::ast::stmt::{Decl, ExportSpec, ForInit, ForTarget, ImportSpec, Stmt};
use js_syntax::ast::{AssignOp, FunctionDecl, Program};
use js_syntax::SourceFile;
use std::sync::Arc;

/// Compile a parsed [`Program`] into a [`BytecodeModule`].
pub fn compile_program(program: &Program) -> DiagResult<BytecodeModule> {
    compile_program_inner(program, None, Vec::new(), Vec::new())
}

/// Compile a program while retaining its source for runtime diagnostics.
pub fn compile_program_with_source(
    program: &Program,
    source: Arc<SourceFile>,
) -> DiagResult<BytecodeModule> {
    compile_program_inner(program, Some(source), Vec::new(), Vec::new())
}

/// Compile runtime eval code with the caller's active private-name
/// environment. The numeric class identities deliberately remain unchanged so
/// the temporary eval frame can reuse the caller's runtime brands.
pub fn compile_eval_program_with_source(
    program: &Program,
    source: Arc<SourceFile>,
    private_names: std::collections::HashMap<String, u32>,
    outer_bindings: Vec<String>,
) -> DiagResult<BytecodeModule> {
    let private_scopes = if private_names.is_empty() {
        Vec::new()
    } else {
        vec![private_names]
    };
    compile_program_inner(program, Some(source), private_scopes, outer_bindings)
}

fn compile_program_inner(
    program: &Program,
    source: Option<Arc<SourceFile>>,
    private_scopes: Vec<std::collections::HashMap<String, u32>>,
    outer_bindings: Vec<String>,
) -> DiagResult<BytecodeModule> {
    let mut ctx = CompilerCtx {
        constants: ConstantPool::new(),
        functions: Vec::new(),
        errors: Vec::new(),
        loops: Vec::new(),
        labels: Vec::new(),
        with_depth: 0,
        function_with_bases: vec![0],
        scopes: Vec::new(),
        private_scopes,
        is_module: program.kind == js_syntax::ast::ProgramKind::Module,
        module_function_initializers: Vec::new(),
        dynamic_import_requests: Vec::new(),
    };
    // Direct eval has a synthetic outer scope whose slot indices correspond to
    // the caller-provided binding cells. Ordinary programs start at <main>.
    if !outer_bindings.is_empty() {
        let mut outer = Scope::default();
        for (index, name) in outer_bindings.into_iter().enumerate() {
            outer.locals.insert(name, index as u16);
        }
        ctx.scopes.push(outer);
    }
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

    main.upvalues = ctx
        .scopes
        .last()
        .unwrap()
        .upvalues
        .iter()
        .map(|binding| binding.spec)
        .collect();
    main.upvalue_names = ctx
        .scopes
        .last()
        .unwrap()
        .upvalues
        .iter()
        .map(|binding| binding.name.clone())
        .collect();

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
        module_function_initializers: ctx.module_function_initializers,
        dynamic_import_requests: ctx.dynamic_import_requests,
        is_module: program.kind == js_syntax::ast::ProgramKind::Module,
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
            js_syntax::ast::ProgramItem::Decl(d) => compile_decl(d, func, ctx, top_level),
        }
    }
}

fn compile_stmt(stmt: &Stmt, func: &mut BytecodeFunction, ctx: &mut CompilerCtx, top_level: bool) {
    compile_stmt_with_labels(stmt, func, ctx, top_level, &[]);
}

fn compile_stmt_with_labels(
    stmt: &Stmt,
    func: &mut BytecodeFunction,
    ctx: &mut CompilerCtx,
    top_level: bool,
    target_labels: &[String],
) {
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
        Stmt::Decl(d) => compile_decl(d, func, ctx, false),
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
            ctx.push_loop(target_labels, true);
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
            ctx.push_loop(target_labels, true);
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
                Some(ForInit::Var(d)) => compile_decl(d, func, ctx, false),
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
            ctx.push_loop(target_labels, true);
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
            ctx.push_loop(&[], false);
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
            ctx.pop_loop(func, end, end);
        }
        Stmt::Break { label, .. } => {
            if !ctx.emit_break(func, label.as_deref()) {
                ctx.errors.push(Diagnostic::error(
                    stmt.span(),
                    "`break` outside of a loop or switch",
                ));
            }
        }
        Stmt::Continue { label, .. } => {
            if !ctx.emit_continue(func, label.as_deref()) {
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
            compile_for_of(left, right, body, func, ctx, target_labels);
        }
        Stmt::ForIn {
            left, right, body, ..
        } => {
            compile_for_in(left, right, body, func, ctx, target_labels);
        }
        Stmt::Labeled { label, body, .. } => {
            let mut labels = vec![label.clone()];
            let mut target = body.as_ref();
            while let Stmt::Labeled {
                label: nested,
                body,
                ..
            } = target
            {
                labels.push(nested.clone());
                target = body.as_ref();
            }
            if is_iteration_statement(target) {
                compile_stmt_with_labels(target, func, ctx, false, &labels);
            } else {
                ctx.push_label(labels);
                compile_stmt(target, func, ctx, false);
                let end = func.here();
                ctx.pop_label(func, end);
            }
        }
        Stmt::With { obj, body, .. } => {
            compile_expr(obj, func, ctx);
            func.emit_bare(Opcode::EnterWith);
            ctx.with_depth += 1;
            compile_stmt(body, func, ctx, false);
            ctx.with_depth -= 1;
            func.emit_bare(Opcode::LeaveWith);
        }
    }
    func.annotate_since(start_pc, stmt.span());
}

fn is_iteration_statement(statement: &Stmt) -> bool {
    matches!(
        statement,
        Stmt::While { .. }
            | Stmt::DoWhile { .. }
            | Stmt::For { .. }
            | Stmt::ForIn { .. }
            | Stmt::ForOf { .. }
    )
}

fn compile_decl(
    decl: &Decl,
    func: &mut BytecodeFunction,
    ctx: &mut CompilerCtx,
    module_level: bool,
) {
    let start_pc = func.code.len();
    match decl {
        Decl::Var {
            kind, declarations, ..
        } => {
            for d in declarations {
                // Ensure the pattern's binding names are in scope (idempotent;
                // the hoisting pre-pass usually did this already).
                declare_pattern_names(&d.name, func, ctx);
                if let Some(init) = &d.init {
                    compile_expr(init, func, ctx); // [value]
                    bind_pattern(&d.name, func, ctx); // consumes
                } else if !matches!(kind, js_syntax::ast::stmt::VarKind::Var) {
                    // Lexical declarations are initialized when evaluation
                    // reaches the declaration, even without an initializer.
                    func.emit_bare(Opcode::LdaUndefined);
                    bind_pattern(&d.name, func, ctx);
                }
            }
        }
        Decl::Function(f) => compile_function_decl(f, func, ctx, module_level),
        Decl::Class(c) => {
            let class = compile_class_value(
                c.span,
                c.name.as_deref(),
                &c.body,
                c.superclass.as_deref(),
                func,
                ctx,
            );
            if let Some(name) = &c.name {
                emit_class_value(class, c.superclass.as_deref(), func, ctx);
                let slot = ctx.declare_local(func, name);
                func.emit(Instruction::new(Opcode::StaLocal, slot));
            }
        }
        Decl::Import { .. } => {}
        Decl::Export { spec, .. } => match spec {
            ExportSpec::Named { .. } | ExportSpec::All { .. } | ExportSpec::ReExport { .. } => {}
            ExportSpec::Decl(inner) => compile_decl(inner, func, ctx, module_level),
            ExportSpec::Default(expr) => {
                compile_named_evaluation(expr, "default", func, ctx);
                let slot = ctx.declare_local(func, crate::module::DEFAULT_EXPORT_LOCAL);
                func.emit(Instruction::new(Opcode::StaLocal, slot));
            }
            ExportSpec::DefaultDecl(inner) => compile_default_decl(inner, func, ctx, module_level),
        },
    }
    func.annotate_since(start_pc, decl.span());
}

fn compile_default_decl(
    decl: &Decl,
    func: &mut BytecodeFunction,
    ctx: &mut CompilerCtx,
    module_level: bool,
) {
    match decl {
        Decl::Function(function) if function.name.is_none() => {
            let id = compile_function_value(
                function.span,
                Some("default"),
                &function.params,
                FunctionBody::Block(&function.body),
                false,
                function.is_async,
                function.is_generator,
                func,
                ctx,
            );
            let slot = ctx.declare_local(func, crate::module::DEFAULT_EXPORT_LOCAL);
            if module_level && ctx.is_module {
                ctx.module_function_initializers.push((slot, id));
            } else {
                func.emit(Instruction::new(Opcode::LdaFunction, id as u16));
                func.emit(Instruction::new(Opcode::StaLocal, slot));
            }
        }
        Decl::Class(class) if class.name.is_none() => {
            let compiled = compile_class_value(
                class.span,
                Some("default"),
                &class.body,
                class.superclass.as_deref(),
                func,
                ctx,
            );
            emit_class_value(compiled, class.superclass.as_deref(), func, ctx);
            let slot = ctx.declare_local(func, crate::module::DEFAULT_EXPORT_LOCAL);
            func.emit(Instruction::new(Opcode::StaLocal, slot));
        }
        _ => compile_decl(decl, func, ctx, module_level),
    }
}

fn compile_function_decl(
    f: &FunctionDecl,
    parent: &mut BytecodeFunction,
    ctx: &mut CompilerCtx,
    module_level: bool,
) {
    let id = compile_function_value(
        f.span,
        f.name.as_deref(),
        &f.params,
        FunctionBody::Block(&f.body),
        false,
        f.is_async,
        f.is_generator,
        parent,
        ctx,
    );
    // Bind the function by name in the enclosing scope.
    if let Some(name) = &f.name {
        let slot = ctx.declare_local(parent, name);
        if module_level && ctx.is_module {
            ctx.module_function_initializers.push((slot, id));
        } else {
            parent.emit(Instruction::new(Opcode::LdaFunction, id as u16));
            parent.emit(Instruction::new(Opcode::StaLocal, slot));
        }
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
    is_async: bool,
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
    nested.is_async = is_async;
    nested.is_generator = is_generator;
    let _ = is_arrow;

    ctx.scopes.push(Scope::default());
    ctx.function_with_bases.push(ctx.with_depth);
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
    capture_visible_environment(ctx);

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
    nested.upvalue_names = ctx
        .scopes
        .last()
        .unwrap()
        .upvalues
        .iter()
        .map(|binding| binding.name.clone())
        .collect();
    nested.annotate_since(0, span);

    ctx.scopes.pop();
    ctx.function_with_bases.pop();
    ctx.functions[id as usize - 1] = nested;
    id
}

struct CompiledClass {
    constructor: u32,
    instance_initializer: Option<u32>,
    static_initializer: Option<u32>,
    computed_keys: Vec<Expr>,
    private_scope: std::collections::HashMap<String, u32>,
}

/// Compile a class into its constructor and hidden element initializers.
fn compile_class_value(
    span: js_syntax::Span,
    name: Option<&str>,
    members: &[js_syntax::ast::expr::ClassMember],
    superclass: Option<&Expr>,
    parent: &mut BytecodeFunction,
    ctx: &mut CompilerCtx,
) -> CompiledClass {
    use js_syntax::ast::expr::{ClassMemberKind, ClassMemberValue};

    // Locate the user constructor, if any.
    let mut ctor_params: Vec<Pat> = Vec::new();
    let mut ctor_body: Vec<Stmt> = Vec::new();
    let mut has_constructor = false;
    for m in members {
        if !m.static_ && matches!(m.kind, ClassMemberKind::Constructor) {
            if let ClassMemberValue::Method(f) = &m.value {
                has_constructor = true;
                ctor_params = f.params.clone();
                ctor_body = f.body.clone();
            }
        }
    }

    // Reserve the constructor id before compiling any element. It is also the
    // stable class-definition component of every private brand in this class.
    let id = (ctx.functions.len() + 1) as u32;
    ctx.functions.push(BytecodeFunction::default());
    let private_names: std::collections::HashMap<_, _> = members
        .iter()
        .filter_map(|member| match &member.key {
            js_syntax::ast::pat::PropKey::Private(name) => Some((name.clone(), id)),
            _ => None,
        })
        .collect();
    ctx.private_scopes.push(private_names.clone());
    let mut computed_keys = Vec::new();
    let computed_indices: Vec<_> = members
        .iter()
        .map(|member| match &member.key {
            js_syntax::ast::pat::PropKey::Computed(expression) => {
                let index = computed_keys.len() as u16;
                computed_keys.push(expression.as_ref().clone());
                Some(index)
            }
            _ => None,
        })
        .collect();

    // Build the user-visible constructor body. Instance fields execute through
    // a separate initializer at the base/derived construction boundary.
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
    capture_visible_environment(ctx);

    if superclass.is_some() && !has_constructor {
        ctor.emit(Instruction::new(Opcode::CallSuper, u16::MAX));
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
    ctor.upvalue_names = ctx
        .scopes
        .last()
        .unwrap()
        .upvalues
        .iter()
        .map(|binding| binding.name.clone())
        .collect();
    ctx.scopes.pop();
    ctx.functions[id as usize - 1] = ctor;

    let has_instance_fields = members
        .iter()
        .any(|member| !member.static_ && matches!(member.value, ClassMemberValue::Field(_)));
    let instance_initializer = has_instance_fields.then(|| {
        let initializer_id = (ctx.functions.len() + 1) as u32;
        ctx.functions.push(BytecodeFunction::default());
        let mut initializer = BytecodeFunction::new(span, "<class-instance-initializer>", 0);
        ctx.scopes.push(Scope::default());
        capture_visible_environment(ctx);
        for (member_index, member) in members.iter().enumerate() {
            if member.static_ {
                continue;
            }
            if let ClassMemberValue::Field(field) = &member.value {
                match field {
                    Some(expression) => {
                        compile_expr(expression, &mut initializer, ctx);
                        emit_field_function_name(expression, member, &mut initializer, ctx);
                    }
                    None => initializer.emit_bare(Opcode::LdaUndefined),
                }
                initializer.emit_bare(Opcode::LdaThis);
                emit_class_element_definition(
                    member,
                    computed_indices[member_index],
                    &mut initializer,
                    ctx,
                    false,
                );
            }
        }
        initializer.emit_bare_at(Opcode::LdaUndefined, span);
        initializer.emit_bare_at(Opcode::Return, span);
        initializer.annotate_since(0, span);
        initializer.upvalues = ctx
            .scopes
            .last()
            .unwrap()
            .upvalues
            .iter()
            .map(|binding| binding.spec)
            .collect();
        initializer.upvalue_names = ctx
            .scopes
            .last()
            .unwrap()
            .upvalues
            .iter()
            .map(|binding| binding.name.clone())
            .collect();
        ctx.scopes.pop();
        ctx.functions[initializer_id as usize - 1] = initializer;
        initializer_id
    });

    let has_static_elements = members.iter().any(|member| {
        member.static_
            || matches!(member.value, ClassMemberValue::StaticBlock(_))
            || matches!(member.value, ClassMemberValue::Method(_))
                && (member.static_ || !matches!(member.kind, ClassMemberKind::Constructor))
    });
    let static_initializer = has_static_elements.then(|| {
        let initializer_id = (ctx.functions.len() + 1) as u32;
        ctx.functions.push(BytecodeFunction::default());
        let mut initializer = BytecodeFunction::new(span, "<class-static-initializer>", 0);
        ctx.scopes.push(Scope::default());

        let mut names = Vec::new();
        for member in members {
            if let ClassMemberValue::StaticBlock(body) = &member.value {
                collect_bindings(body, &mut names);
            }
        }
        for name in names {
            ctx.declare_local(&mut initializer, &name);
        }
        capture_visible_environment(ctx);

        let mut class_methods = Vec::new();
        for (member_index, member) in members.iter().enumerate() {
            if !member.static_ && matches!(member.kind, ClassMemberKind::Constructor) {
                continue;
            }
            if let ClassMemberValue::Method(method) = &member.value {
                let method_name = match &member.key {
                    js_syntax::ast::pat::PropKey::Ident(name)
                    | js_syntax::ast::pat::PropKey::String(name) => Some(name.clone()),
                    js_syntax::ast::pat::PropKey::Private(name) => Some(format!("#{name}")),
                    _ => None,
                };
                let method_id = compile_function_value(
                    method.span,
                    method_name.as_deref(),
                    &method.params,
                    FunctionBody::Block(&method.body),
                    false,
                    method.is_async,
                    method.is_generator,
                    &mut initializer,
                    ctx,
                );
                class_methods.push((member, computed_indices[member_index], method_id));
            }
        }
        for (member, computed_key, method_id) in class_methods {
            initializer.emit(Instruction::new(Opcode::LdaFunction, method_id as u16));
            initializer.emit_bare(Opcode::LdaThis);
            if member.static_ {
                emit_class_element_definition(member, computed_key, &mut initializer, ctx, true);
            } else {
                emit_instance_method_definition(member, computed_key, &mut initializer, ctx);
            }
        }
        for (member_index, member) in members.iter().enumerate() {
            match &member.value {
                ClassMemberValue::Field(field) if member.static_ => {
                    match field {
                        Some(expression) => {
                            compile_expr(expression, &mut initializer, ctx);
                            emit_field_function_name(expression, member, &mut initializer, ctx);
                        }
                        None => initializer.emit_bare(Opcode::LdaUndefined),
                    }
                    initializer.emit_bare(Opcode::LdaThis);
                    emit_class_element_definition(
                        member,
                        computed_indices[member_index],
                        &mut initializer,
                        ctx,
                        false,
                    );
                }
                ClassMemberValue::StaticBlock(body) => {
                    compile_stmt_list_body(body, &mut initializer, ctx)
                }
                _ => {}
            }
        }
        initializer.emit_bare_at(Opcode::LdaUndefined, span);
        initializer.emit_bare_at(Opcode::Return, span);
        initializer.annotate_since(0, span);
        initializer.upvalues = ctx
            .scopes
            .last()
            .unwrap()
            .upvalues
            .iter()
            .map(|binding| binding.spec)
            .collect();
        initializer.upvalue_names = ctx
            .scopes
            .last()
            .unwrap()
            .upvalues
            .iter()
            .map(|binding| binding.name.clone())
            .collect();
        ctx.scopes.pop();
        ctx.functions[initializer_id as usize - 1] = initializer;
        initializer_id
    });

    ctx.private_scopes.pop();
    let _ = parent;
    CompiledClass {
        constructor: id,
        instance_initializer,
        static_initializer,
        computed_keys,
        private_scope: private_names,
    }
}

fn emit_field_function_name(
    expression: &Expr,
    member: &js_syntax::ast::expr::ClassMember,
    func: &mut BytecodeFunction,
    ctx: &mut CompilerCtx,
) {
    let expression = match expression {
        Expr::Paren { expr, .. } => expr.as_ref(),
        expression => expression,
    };
    let anonymous = match expression {
        Expr::Function(function) => function.name.is_none(),
        Expr::Arrow(_) => true,
        Expr::Class(class) => class.name.is_none(),
        _ => false,
    };
    if !anonymous {
        return;
    }
    let name = match &member.key {
        js_syntax::ast::pat::PropKey::Ident(name) | js_syntax::ast::pat::PropKey::String(name) => {
            name.clone()
        }
        js_syntax::ast::pat::PropKey::Private(name) => format!("#{name}"),
        _ => return,
    };
    let name = ctx.constants.intern_str(name);
    func.emit(Instruction::new(Opcode::SetFunctionName, name));
}

fn emit_instance_method_definition(
    member: &js_syntax::ast::expr::ClassMember,
    computed_key: Option<u16>,
    func: &mut BytecodeFunction,
    ctx: &mut CompilerCtx,
) {
    use js_syntax::ast::expr::ClassMemberKind;
    if let js_syntax::ast::pat::PropKey::Private(name) = &member.key {
        let private = ctx.private_name_constant(name, member.span);
        let opcode = match member.kind {
            ClassMemberKind::Get => Opcode::DefinePrivateGetterTemplate,
            ClassMemberKind::Set => Opcode::DefinePrivateSetterTemplate,
            _ => Opcode::DefinePrivateMethodTemplate,
        };
        func.emit(Instruction::new(opcode, private));
        return;
    }

    let prototype = ctx.constants.intern_str("prototype");
    func.emit(Instruction::new(Opcode::LdaConst, prototype));
    func.emit_bare(Opcode::GetProp);
    emit_class_element_key(member, computed_key, func, ctx);
    func.emit_bare(match member.kind {
        ClassMemberKind::Get => Opcode::DefineGetter,
        ClassMemberKind::Set => Opcode::DefineSetter,
        _ => Opcode::DefineMethod,
    });
}

fn emit_class_element_definition(
    member: &js_syntax::ast::expr::ClassMember,
    computed_key: Option<u16>,
    func: &mut BytecodeFunction,
    ctx: &mut CompilerCtx,
    method: bool,
) {
    use js_syntax::ast::expr::ClassMemberKind;
    if let js_syntax::ast::pat::PropKey::Private(name) = &member.key {
        let private = ctx.private_name_constant(name, member.span);
        let opcode = match member.kind {
            ClassMemberKind::Get => Opcode::DefinePrivateGetter,
            ClassMemberKind::Set => Opcode::DefinePrivateSetter,
            _ if method => Opcode::DefinePrivateMethod,
            _ => Opcode::DefinePrivate,
        };
        func.emit(Instruction::new(opcode, private));
    } else {
        emit_class_element_key(member, computed_key, func, ctx);
        func.emit_bare(match member.kind {
            ClassMemberKind::Get => Opcode::DefineGetter,
            ClassMemberKind::Set => Opcode::DefineSetter,
            _ if method => Opcode::DefineMethod,
            _ => Opcode::DefineDataProperty,
        });
    }
}

fn emit_class_element_key(
    member: &js_syntax::ast::expr::ClassMember,
    computed_key: Option<u16>,
    func: &mut BytecodeFunction,
    ctx: &mut CompilerCtx,
) {
    if let Some(index) = computed_key {
        func.emit(Instruction::new(Opcode::LoadClassFieldKey, index));
    } else {
        compile_prop_key_push(&member.key, member.computed, func, ctx);
    }
}

fn emit_class_value(
    class: CompiledClass,
    superclass: Option<&Expr>,
    func: &mut BytecodeFunction,
    ctx: &mut CompilerCtx,
) {
    if let Some(superclass) = superclass {
        compile_expr(superclass, func, ctx);
    }
    func.emit(Instruction::new(
        Opcode::LdaFunction,
        class.constructor as u16,
    ));
    if let Some(initializer) = class.instance_initializer {
        func.emit(Instruction::new(Opcode::LdaFunction, initializer as u16));
        func.emit_bare(Opcode::SetClassInstanceInitializer);
    }
    if superclass.is_some() {
        func.emit_bare(Opcode::SetClassHeritage);
    }
    func.emit_bare(Opcode::ActivateClassPrivateEnvironment);
    ctx.private_scopes.push(class.private_scope);
    for expression in &class.computed_keys {
        func.emit_bare(Opcode::Dup);
        compile_expr(expression, func, ctx);
        func.emit_bare(Opcode::DefineClassFieldKey);
    }
    ctx.private_scopes.pop();
    if let Some(initializer) = class.static_initializer {
        func.emit_bare(Opcode::Dup);
        func.emit(Instruction::new(Opcode::LdaFunction, initializer as u16));
        func.emit(Instruction::new(Opcode::CallMethod, 0));
        func.emit_bare(Opcode::Pop);
    }
    func.emit_bare(Opcode::DeactivateClassPrivateEnvironment);
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
        Expr::Unary { op, arg, .. } => match op {
            js_syntax::ast::op::UnaryOp::Delete => compile_delete(arg, func, ctx),
            js_syntax::ast::op::UnaryOp::Typeof => compile_typeof(arg, func, ctx),
            _ => {
                compile_expr(arg, func, ctx);
                func.emit_bare(
                    Opcode::for_unaryop(*op).expect("non-delete unary opcode must lower"),
                );
            }
        },
        Expr::Binary {
            op, left, right, ..
        } => {
            compile_expr(left, func, ctx);
            compile_expr(right, func, ctx);
            emit_binop(*op, func);
        }
        Expr::PrivateIn {
            name, right, span, ..
        } => {
            compile_expr(right, func, ctx);
            let private = ctx.private_name_constant(name, *span);
            func.emit(Instruction::new(Opcode::PrivateIn, private));
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
        Expr::TaggedTemplate { tag, template, .. } => {
            compile_tagged_template(tag, template, func, ctx);
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
                match &p.value {
                    ObjectPropValue::Spread(source) => {
                        compile_expr(source, func, ctx);
                        func.emit_bare(Opcode::CopyDataProperties);
                    }
                    ObjectPropValue::Expr(value) => {
                        func.emit_bare(Opcode::Dup);
                        compile_expr(value, func, ctx);
                        func.emit_bare(Opcode::Swap);
                        compile_prop_key_push(&p.key, p.computed, func, ctx);
                        func.emit_bare(Opcode::SetProp);
                    }
                    ObjectPropValue::Method(method) => {
                        let name = match &p.key {
                            js_syntax::ast::pat::PropKey::Ident(name)
                            | js_syntax::ast::pat::PropKey::String(name)
                            | js_syntax::ast::pat::PropKey::Private(name) => Some(name.as_str()),
                            _ => None,
                        };
                        let id = compile_function_value(
                            method.span,
                            name,
                            &method.params,
                            FunctionBody::Block(&method.body),
                            false,
                            method.is_async,
                            method.is_generator,
                            func,
                            ctx,
                        );
                        func.emit_bare(Opcode::Dup);
                        func.emit(Instruction::new(Opcode::LdaFunction, id as u16));
                        func.emit_bare(Opcode::Swap);
                        compile_prop_key_push(&p.key, p.computed, func, ctx);
                        let opcode = match p.kind {
                            js_syntax::ast::expr::ObjectPropKind::Get => Opcode::DefineGetter,
                            js_syntax::ast::expr::ObjectPropKind::Set => Opcode::DefineSetter,
                            js_syntax::ast::expr::ObjectPropKind::Init => Opcode::SetProp,
                        };
                        func.emit_bare(opcode);
                    }
                }
            }
        }
        Expr::Member(m) => {
            if has_optional_member_chain(expr) {
                compile_optional_member_chain(expr, func, ctx);
            } else if matches!(m.object.as_ref(), Expr::Super(_)) {
                compile_member_key_push(&m.property, func, ctx);
                func.emit_bare(Opcode::GetSuperProp);
            } else {
                compile_expr(&m.object, func, ctx);
                if let MemberProp::Private(name) = &m.property {
                    let private = ctx.private_name_constant(name, m.span);
                    func.emit(Instruction::new(Opcode::GetPrivate, private));
                } else {
                    compile_member_key_push(&m.property, func, ctx);
                    func.emit_bare(Opcode::GetProp);
                }
            }
        }
        Expr::New(n) => {
            compile_expr(&n.callee, func, ctx);
            if n.args.iter().any(|arg| matches!(arg, CallArg::Spread(_))) {
                compile_argument_list(&n.args, func, ctx);
                func.emit_bare(Opcode::NewWithArgumentList);
                func.annotate_since(start_pc, expr.span());
                return;
            }
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
        Expr::Sequence { exprs, .. } => {
            for (index, expression) in exprs.iter().enumerate() {
                compile_expr(expression, func, ctx);
                if index + 1 != exprs.len() {
                    func.emit_bare(Opcode::Pop);
                }
            }
            if exprs.is_empty() {
                func.emit_bare(Opcode::LdaUndefined);
            }
        }
        Expr::Assign {
            op, left, right, ..
        } => {
            if matches!(op, AssignOp::And | AssignOp::Or | AssignOp::Nullish) {
                compile_logical_assignment(*op, left, right, func, ctx);
                func.annotate_since(start_pc, expr.span());
                return;
            }
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
                        if matches!(m.object.as_ref(), Expr::Super(_)) {
                            compile_member_key_push(&m.property, func, ctx);
                            func.emit_bare(Opcode::SetSuperProp);
                        } else if let MemberProp::Private(name) = &m.property {
                            compile_expr(&m.object, func, ctx); // [v, v, obj]
                            let private = ctx.private_name_constant(name, m.span);
                            func.emit(Instruction::new(Opcode::SetPrivate, private));
                        } else {
                            compile_expr(&m.object, func, ctx); // [v, v, obj]
                            compile_member_key_push(&m.property, func, ctx); // [v, v, obj, key]
                            func.emit_bare(Opcode::SetProp); // [v]
                        }
                    } else {
                        let target = prepare_assign_target(left, func, ctx)
                            .expect("member assignment target must prepare");
                        load_prepared_assignment(&target, func, ctx);
                        compile_expr(right, func, ctx);
                        emit_binop(compound_to_binop(*op), func);
                        func.emit_bare(Opcode::Dup);
                        put_prepared_assignment(target, func, ctx);
                    }
                }
                AssignTarget::Pat(pattern) => {
                    if *op != AssignOp::Assign {
                        unreachable!("destructuring targets only permit simple assignment");
                    }
                    compile_expr(right, func, ctx);
                    // Assignment expressions preserve the RHS value while the
                    // pattern consumes a second copy.
                    func.emit_bare(Opcode::Dup);
                    assign_pattern(pattern, func, ctx);
                }
            }
        }
        Expr::Call(call) => {
            // Method call `obj.m(args)` / `obj[k](args)`: keep `obj` as `this`.
            match call.callee.as_ref() {
                Expr::Super(_) => {
                    if call
                        .args
                        .iter()
                        .any(|arg| matches!(arg, CallArg::Spread(_)))
                    {
                        compile_argument_list(&call.args, func, ctx);
                        func.emit_bare(Opcode::CallSuperWithArgumentList);
                        func.annotate_since(start_pc, expr.span());
                        return;
                    }
                    let mut count = 0u16;
                    for argument in &call.args {
                        match argument {
                            CallArg::Expr(expression) => compile_expr(expression, func, ctx),
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
                    func.emit(Instruction::new(Opcode::CallSuper, count));
                }
                Expr::Member(m) => {
                    if matches!(m.object.as_ref(), Expr::Super(_)) {
                        // A super property Reference has the current `this`
                        // value as its receiver, but resolves the method from
                        // the active method's [[HomeObject]] prototype.
                        func.emit_bare(Opcode::LdaThis); // [receiver]
                        compile_member_key_push(&m.property, func, ctx);
                        func.emit_bare(Opcode::GetSuperProp); // [receiver, method]
                    } else {
                        compile_expr(&m.object, func, ctx); // [obj]
                        func.emit_bare(Opcode::Dup); // [obj, obj]
                        if let MemberProp::Private(name) = &m.property {
                            let private = ctx.private_name_constant(name, m.span);
                            func.emit(Instruction::new(Opcode::GetPrivate, private));
                        } else {
                            compile_member_key_push(&m.property, func, ctx); // [obj, obj, key]
                            func.emit_bare(Opcode::GetProp); // [obj, method]
                        }
                    }
                    if call
                        .args
                        .iter()
                        .any(|arg| matches!(arg, CallArg::Spread(_)))
                    {
                        compile_argument_list(&call.args, func, ctx);
                        func.emit_bare(Opcode::CallMethodWithArgumentList);
                        func.annotate_since(start_pc, expr.span());
                        return;
                    }
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
                    if call
                        .args
                        .iter()
                        .any(|arg| matches!(arg, CallArg::Spread(_)))
                    {
                        compile_argument_list(&call.args, func, ctx);
                        let opcode = if matches!(
                            call.callee.as_ref(),
                            Expr::Ident { name, .. } if name == "eval"
                        ) {
                            Opcode::CallDirectEvalWithArgumentList
                        } else {
                            Opcode::CallWithArgumentList
                        };
                        func.emit_bare(opcode);
                        func.annotate_since(start_pc, expr.span());
                        return;
                    }
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
                    let opcode = if matches!(
                        call.callee.as_ref(),
                        Expr::Ident { name, .. } if name == "eval"
                    ) {
                        Opcode::CallDirectEval
                    } else {
                        Opcode::Call
                    };
                    func.emit(Instruction::new(opcode, count));
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
                f.is_async,
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
            let id = compile_function_value(
                a.span, None, &a.params, body, true, a.is_async, false, func, ctx,
            );
            func.emit(Instruction::new(Opcode::LdaFunction, id as u16));
        }
        Expr::Class(c) => {
            let class = compile_class_value(
                c.span,
                c.name.as_deref(),
                &c.body,
                c.superclass.as_deref(),
                func,
                ctx,
            );
            emit_class_value(class, c.superclass.as_deref(), func, ctx);
        }
        Expr::This { .. } => func.emit_bare(Opcode::LdaThis),
        // The VM does not expose a new-target register yet. Preserve the
        // previous behavior for ordinary calls until constructor frames carry it.
        Expr::NewTarget(_) => func.emit_bare(Opcode::LdaUndefined),
        Expr::Yield { arg, delegate, .. } => {
            // Push the operand (undefined only for plain `yield`), then let the
            // VM suspend directly or enter the persistent delegation machine.
            match arg {
                Some(e) => compile_expr(e, func, ctx),
                None => func.emit_bare(Opcode::LdaUndefined),
            }
            func.emit_bare(if *delegate {
                Opcode::YieldStar
            } else {
                Opcode::Yield
            });
        }
        Expr::Await { arg, .. } => {
            compile_expr(arg, func, ctx);
            func.emit_bare(Opcode::Await);
        }
        Expr::ImportCall { source, .. } => {
            if let Expr::Lit(Lit::String(_, specifier, _)) = source.as_ref() {
                if !ctx.dynamic_import_requests.contains(specifier) {
                    ctx.dynamic_import_requests.push(specifier.clone());
                }
            }
            compile_expr(source, func, ctx);
            func.emit_bare(Opcode::DynamicImport);
        }
        Expr::ImportMeta(_) => func.emit_bare(Opcode::GetImportMeta),
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

fn has_optional_member_chain(expression: &Expr) -> bool {
    match expression {
        Expr::Member(member) => {
            member.optional || has_optional_member_chain(member.object.as_ref())
        }
        _ => false,
    }
}

fn collect_member_chain<'a>(expression: &'a Expr, members: &mut Vec<&'a MemberExpr>) -> &'a Expr {
    if let Expr::Member(member) = expression {
        let root = collect_member_chain(member.object.as_ref(), members);
        members.push(member);
        root
    } else {
        expression
    }
}

fn compile_optional_member_chain(
    expression: &Expr,
    func: &mut BytecodeFunction,
    ctx: &mut CompilerCtx,
) {
    let mut members = Vec::new();
    let root = collect_member_chain(expression, &mut members);
    let mut member_index = 0;
    if matches!(root, Expr::Super(_)) {
        let first = members
            .first()
            .expect("a super optional member chain has a property");
        compile_member_key_push(&first.property, func, ctx);
        func.emit_bare(Opcode::GetSuperProp);
        member_index = 1;
    } else {
        compile_expr(root, func, ctx);
    }
    let mut nullish_jumps = Vec::new();
    for member in members.into_iter().skip(member_index) {
        if member.optional {
            func.emit_bare(Opcode::Dup);
            nullish_jumps.push(emit_placeholder(func, Opcode::JumpIfNullish));
        }
        if let MemberProp::Private(name) = &member.property {
            let private = ctx.private_name_constant(name.as_str(), member.span);
            func.emit(Instruction::new(Opcode::GetPrivate, private));
        } else {
            compile_member_key_push(&member.property, func, ctx);
            func.emit_bare(Opcode::GetProp);
        }
    }
    let completed = emit_placeholder(func, Opcode::Jump);
    let nullish = func.here();
    for jump in nullish_jumps {
        patch(func, jump, nullish);
    }
    func.emit_bare(Opcode::Pop);
    func.emit_bare(Opcode::LdaUndefined);
    patch(func, completed, func.here());
}

/// Compile the specification's NamedEvaluation operation. The transparent
/// parenthesis recursion is significant for `export default (function() {})`.
fn compile_named_evaluation(
    expr: &Expr,
    inferred_name: &str,
    func: &mut BytecodeFunction,
    ctx: &mut CompilerCtx,
) {
    match expr {
        Expr::Paren { expr, .. } => compile_named_evaluation(expr, inferred_name, func, ctx),
        Expr::Function(function) if function.name.is_none() => {
            let id = compile_function_value(
                function.span,
                Some(inferred_name),
                &function.params,
                FunctionBody::Block(&function.body),
                false,
                function.is_async,
                function.is_generator,
                func,
                ctx,
            );
            func.emit(Instruction::new(Opcode::LdaFunction, id as u16));
        }
        Expr::Arrow(arrow) => {
            let body = match &arrow.body {
                js_syntax::ast::expr::ArrowBody::Block(statements) => {
                    FunctionBody::Block(statements)
                }
                js_syntax::ast::expr::ArrowBody::Expr(expression) => FunctionBody::Expr(expression),
            };
            let id = compile_function_value(
                arrow.span,
                Some(inferred_name),
                &arrow.params,
                body,
                true,
                arrow.is_async,
                false,
                func,
                ctx,
            );
            func.emit(Instruction::new(Opcode::LdaFunction, id as u16));
        }
        Expr::Class(class) if class.name.is_none() => {
            let compiled = compile_class_value(
                class.span,
                Some(inferred_name),
                &class.body,
                class.superclass.as_deref(),
                func,
                ctx,
            );
            emit_class_value(compiled, class.superclass.as_deref(), func, ctx);
        }
        _ => compile_expr(expr, func, ctx),
    }
}

/// `typeof` is the one identifier operation that does not call GetValue for an
/// unresolvable reference. Encode that distinction directly in bytecode.
fn compile_typeof(arg: &Expr, func: &mut BytecodeFunction, ctx: &mut CompilerCtx) {
    if let Expr::Ident { name, .. } = arg {
        let reference = ctx.resolve_var(name);
        if ctx.uses_dynamic_name(&reference) {
            let name = ctx.constants.intern_str(name);
            func.emit(Instruction::new(Opcode::TypeofName, name));
            return;
        }
        if name != "undefined" && matches!(reference, VarRef::Global) {
            let name = ctx.constants.intern_str(name);
            func.emit(Instruction::new(Opcode::TypeofGlobal, name));
            return;
        }
    }
    compile_expr(arg, func, ctx);
    func.emit_bare(Opcode::Typeof);
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
        Lit::BigInt(_, raw) => {
            let idx = ctx.constants.intern_bigint(raw);
            func.emit(Instruction::new(Opcode::LdaConst, idx));
        }
        Lit::TemplateString { cooked, raw, .. } => {
            let value = cooked.as_ref().unwrap_or(raw);
            let idx = ctx.constants.intern_str(value);
            func.emit(Instruction::new(Opcode::LdaConst, idx));
        }
        Lit::Regex { span, .. } => {
            ctx.errors.push(Diagnostic::error(
                *span,
                "this literal kind is not supported yet",
            ));
            func.emit_bare(Opcode::LdaUndefined);
        }
    }
}

fn compile_tagged_template(
    tag: &Expr,
    template: &Expr,
    func: &mut BytecodeFunction,
    ctx: &mut CompilerCtx,
) {
    let method_call = match tag {
        Expr::Member(member) if matches!(member.object.as_ref(), Expr::Super(_)) => {
            func.emit_bare(Opcode::LdaThis);
            compile_member_key_push(&member.property, func, ctx);
            func.emit_bare(Opcode::GetSuperProp);
            true
        }
        Expr::Member(member) => {
            compile_expr(&member.object, func, ctx);
            func.emit_bare(Opcode::Dup);
            if let MemberProp::Private(name) = &member.property {
                let private = ctx.private_name_constant(name, member.span);
                func.emit(Instruction::new(Opcode::GetPrivate, private));
            } else {
                compile_member_key_push(&member.property, func, ctx);
                func.emit_bare(Opcode::GetProp);
            }
            true
        }
        _ => {
            compile_expr(tag, func, ctx);
            false
        }
    };

    let (cooked, raw, expressions) = match template {
        Expr::Lit(Lit::TemplateString { cooked, raw, .. }) => {
            (vec![cooked.clone()], vec![raw.clone()], &[][..])
        }
        Expr::TemplateLit {
            quasis,
            expressions,
            ..
        } => (
            quasis.iter().map(|(cooked, _)| cooked.clone()).collect(),
            quasis.iter().map(|(_, raw)| raw.clone()).collect(),
            expressions.as_slice(),
        ),
        _ => {
            ctx.errors.push(Diagnostic::error(
                template.span(),
                "tagged template operand is not a template literal",
            ));
            func.emit_bare(Opcode::LdaUndefined);
            return;
        }
    };
    let site = func.template_sites.len() as u16;
    func.template_sites
        .push(crate::module::TemplateSite { cooked, raw });
    func.emit(Instruction::new(Opcode::GetTemplateObject, site));
    for expression in expressions {
        compile_expr(expression, func, ctx);
    }
    let argc = expressions.len() as u16 + 1;
    func.emit(Instruction::new(
        if method_call {
            Opcode::CallMethod
        } else {
            Opcode::Call
        },
        argc,
    ));
}

fn compile_argument_list(
    arguments: &[CallArg],
    func: &mut BytecodeFunction,
    ctx: &mut CompilerCtx,
) {
    func.emit(Instruction::new(Opcode::NewArray, 0));
    for argument in arguments {
        match argument {
            CallArg::Expr(expression) => {
                compile_expr(expression, func, ctx);
                func.emit_bare(Opcode::ArrayPush);
            }
            CallArg::Spread(expression) => {
                compile_expr(expression, func, ctx);
                func.emit_bare(Opcode::GetIterator);
                let iterator = fresh_temp(func, "argument-iterator");
                func.emit(Instruction::new(Opcode::StaLocal, iterator));
                let next = func.here();
                func.emit(Instruction::new(Opcode::LdaLocal, iterator));
                func.emit_bare(Opcode::IterNext);
                func.emit_bare(Opcode::Dup);
                let done = ctx.constants.intern_str("done");
                func.emit(Instruction::new(Opcode::LdaConst, done));
                func.emit_bare(Opcode::GetProp);
                let completed = emit_placeholder(func, Opcode::JumpIfTrue);
                let value = ctx.constants.intern_str("value");
                func.emit(Instruction::new(Opcode::LdaConst, value));
                func.emit_bare(Opcode::GetProp);
                func.emit_bare(Opcode::ArrayPush);
                emit_jump(func, Opcode::Jump, next);
                patch(func, completed, func.here());
                func.emit_bare(Opcode::Pop);
            }
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
        Ushr => func.emit_bare(Opcode::Ushr),
        Instanceof => func.emit_bare(Opcode::Instanceof),
        In => func.emit_bare(Opcode::In),
        // These normally arrive as `Expr::Logical` and are lowered with
        // control flow. Keep explicit opcodes here so malformed AST input can
        // never silently acquire arithmetic semantics.
        And => func.emit_bare(Opcode::LogicalAnd),
        Or => func.emit_bare(Opcode::LogicalOr),
        NullishCoal => func.emit_bare(Opcode::NullishCoal),
    }
}

/// Lower `delete` according to the kind of Reference produced by its operand.
/// A property reference needs both base and key; an environment binding is not
/// configurable; an unresolvable/global reference is handled without first
/// performing `GetValue`, and a non-reference expression evaluates only for
/// side effects before producing `true`.
fn compile_delete(arg: &Expr, func: &mut BytecodeFunction, ctx: &mut CompilerCtx) {
    match arg {
        Expr::Member(member) => {
            if matches!(member.object.as_ref(), Expr::Super(_)) {
                compile_member_key_push(&member.property, func, ctx);
                func.emit_bare(Opcode::DeleteSuperProp);
            } else {
                compile_expr(&member.object, func, ctx);
                compile_member_key_push(&member.property, func, ctx);
                func.emit_bare(Opcode::DeleteProp);
            }
        }
        Expr::Ident { name, .. } => match ctx.resolve_var(name) {
            reference if ctx.uses_dynamic_name(&reference) => {
                let name = ctx.constants.intern_str(name);
                func.emit(Instruction::new(Opcode::DeleteName, name));
            }
            VarRef::Local(_) | VarRef::Upvalue(_) => func.emit_bare(Opcode::LdaFalse),
            VarRef::Global => {
                let name = ctx.constants.intern_str(name);
                func.emit(Instruction::new(Opcode::DeleteGlobal, name));
            }
        },
        _ => {
            compile_expr(arg, func, ctx);
            func.emit_bare(Opcode::Pop);
            func.emit_bare(Opcode::LdaTrue);
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

fn assign_for_target(target: &ForTarget, func: &mut BytecodeFunction, ctx: &mut CompilerCtx) {
    match target {
        ForTarget::Var(_) => bind_pattern(for_target_pat(target), func, ctx),
        ForTarget::Pat(pattern) => assign_pattern(pattern, func, ctx),
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
    target_labels: &[String],
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
    assign_for_target(left, func, ctx);
    // body
    ctx.push_loop(target_labels, true);
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
    target_labels: &[String],
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
    assign_for_target(left, func, ctx);
    ctx.push_loop(target_labels, true);
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
        Pat::Ident { name, .. } => store_binding_ident(name, func, ctx),
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

/// A Reference Record lowered into stable temporary slots. Destructuring must
/// evaluate a non-pattern target before reading the source property/iterator,
/// while PutValue happens afterwards.
enum PreparedAssignmentTarget {
    Ident(String),
    Private { object: u16, private_name: u16 },
    Property { object: u16, key: u16 },
    Super { key: u16 },
}

fn fresh_temp(func: &mut BytecodeFunction, purpose: &str) -> u16 {
    let next = func.locals.slot_count();
    func.locals.intern(format!("<{purpose}-{next}>"))
}

/// Evaluate the Reference part of a destructuring leaf without fetching or
/// writing its value. This implements the ordering required by
/// Keyed/IteratorDestructuringAssignmentEvaluation.
fn prepare_assignment_target(
    pat: &Pat,
    func: &mut BytecodeFunction,
    ctx: &mut CompilerCtx,
) -> Option<PreparedAssignmentTarget> {
    match pat {
        Pat::Ident { name, .. } => Some(PreparedAssignmentTarget::Ident(name.clone())),
        Pat::Assignment { left, .. } | Pat::Rest { arg: left, .. } => {
            prepare_assignment_target(left, func, ctx)
        }
        Pat::Member(member) => {
            if matches!(member.object.as_ref(), Expr::Super(_)) {
                let key = fresh_temp(func, "destr-ref-super-key");
                compile_member_key_push(&member.property, func, ctx);
                func.emit(Instruction::new(Opcode::StaLocal, key));
                return Some(PreparedAssignmentTarget::Super { key });
            }
            let object = fresh_temp(func, "destr-ref-object");
            compile_expr(&member.object, func, ctx);
            func.emit(Instruction::new(Opcode::StaLocal, object));
            if let MemberProp::Private(name) = &member.property {
                let private_name = ctx.private_name_constant(name, member.span);
                Some(PreparedAssignmentTarget::Private {
                    object,
                    private_name,
                })
            } else {
                let key = fresh_temp(func, "destr-ref-key");
                compile_member_key_push(&member.property, func, ctx);
                func.emit(Instruction::new(Opcode::StaLocal, key));
                Some(PreparedAssignmentTarget::Property { object, key })
            }
        }
        Pat::Array { .. } | Pat::Object { .. } => None,
    }
}

fn put_prepared_assignment(
    target: PreparedAssignmentTarget,
    func: &mut BytecodeFunction,
    ctx: &mut CompilerCtx,
) {
    match target {
        PreparedAssignmentTarget::Ident(name) => store_ident(&name, func, ctx),
        PreparedAssignmentTarget::Private {
            object,
            private_name,
        } => {
            func.emit(Instruction::new(Opcode::LdaLocal, object));
            func.emit(Instruction::new(Opcode::SetPrivate, private_name));
        }
        PreparedAssignmentTarget::Property { object, key } => {
            func.emit(Instruction::new(Opcode::LdaLocal, object));
            func.emit(Instruction::new(Opcode::LdaLocal, key));
            func.emit_bare(Opcode::SetProp);
        }
        PreparedAssignmentTarget::Super { key } => {
            func.emit(Instruction::new(Opcode::LdaLocal, key));
            func.emit_bare(Opcode::SetSuperProp);
        }
    }
}

fn prepare_assign_target(
    target: &AssignTarget,
    func: &mut BytecodeFunction,
    ctx: &mut CompilerCtx,
) -> Option<PreparedAssignmentTarget> {
    match target {
        AssignTarget::Ident { name, .. } => Some(PreparedAssignmentTarget::Ident(name.clone())),
        AssignTarget::Member(member) => {
            prepare_assignment_target(&Pat::Member(member.clone()), func, ctx)
        }
        AssignTarget::Pat(_) => None,
    }
}

fn load_prepared_assignment(
    target: &PreparedAssignmentTarget,
    func: &mut BytecodeFunction,
    ctx: &mut CompilerCtx,
) {
    match target {
        PreparedAssignmentTarget::Ident(name) => load_ident(name, func, ctx),
        PreparedAssignmentTarget::Private {
            object,
            private_name,
        } => {
            func.emit(Instruction::new(Opcode::LdaLocal, *object));
            func.emit(Instruction::new(Opcode::GetPrivate, *private_name));
        }
        PreparedAssignmentTarget::Property { object, key } => {
            func.emit(Instruction::new(Opcode::LdaLocal, *object));
            func.emit(Instruction::new(Opcode::LdaLocal, *key));
            func.emit_bare(Opcode::GetProp);
        }
        PreparedAssignmentTarget::Super { key } => {
            func.emit(Instruction::new(Opcode::LdaLocal, *key));
            func.emit_bare(Opcode::GetSuperProp);
        }
    }
}

fn assignment_target_name(target: &AssignTarget) -> Option<String> {
    match target {
        AssignTarget::Ident { name, .. } => Some(name.clone()),
        AssignTarget::Member(member) => match &member.property {
            MemberProp::Ident(name) => Some(name.clone()),
            MemberProp::Private(name) => Some(format!("#{name}")),
            MemberProp::Computed(_) => None,
        },
        AssignTarget::Pat(_) => None,
    }
}

fn compile_logical_assignment(
    op: AssignOp,
    left: &AssignTarget,
    right: &Expr,
    func: &mut BytecodeFunction,
    ctx: &mut CompilerCtx,
) {
    let Some(target) = prepare_assign_target(left, func, ctx) else {
        ctx.errors.push(Diagnostic::error(
            right.span(),
            "logical assignment requires a simple assignment target",
        ));
        func.emit_bare(Opcode::LdaUndefined);
        return;
    };
    load_prepared_assignment(&target, func, ctx);
    func.emit_bare(Opcode::Dup);

    let short_circuit = match op {
        AssignOp::And => Some(emit_placeholder(func, Opcode::JumpIfFalse)),
        AssignOp::Or => Some(emit_placeholder(func, Opcode::JumpIfTrue)),
        AssignOp::Nullish => None,
        _ => unreachable!("non-logical assignment passed to logical lowering"),
    };
    let nullish_rhs = if op == AssignOp::Nullish {
        Some(emit_placeholder(func, Opcode::JumpIfNullish))
    } else {
        None
    };
    let non_nullish_end = nullish_rhs.map(|_| emit_placeholder(func, Opcode::Jump));
    let rhs_start = func.here();
    if let Some(jump) = nullish_rhs {
        patch(func, jump, rhs_start);
    }
    func.emit_bare(Opcode::Pop);
    if let Some(name) = assignment_target_name(left) {
        compile_named_evaluation(right, &name, func, ctx);
    } else {
        compile_expr(right, func, ctx);
    }
    func.emit_bare(Opcode::Dup);
    put_prepared_assignment(target, func, ctx);
    let end = func.here();
    if let Some(jump) = short_circuit {
        patch(func, jump, end);
    }
    if let Some(jump) = non_nullish_end {
        patch(func, jump, end);
    }
}

fn assign_prepared_value(
    pat: &Pat,
    prepared: Option<PreparedAssignmentTarget>,
    func: &mut BytecodeFunction,
    ctx: &mut CompilerCtx,
) {
    if let Pat::Assignment { left, right, .. } = pat {
        func.emit_bare(Opcode::Dup);
        func.emit_bare(Opcode::LdaUndefined);
        func.emit_bare(Opcode::StrictEq);
        let keep = emit_placeholder(func, Opcode::JumpIfFalse);
        func.emit_bare(Opcode::Pop);
        compile_expr(right, func, ctx);
        patch(func, keep, func.here());
        if let Some(target) = prepared {
            put_prepared_assignment(target, func, ctx);
        } else {
            assign_pattern(left, func, ctx);
        }
    } else if let Some(target) = prepared {
        put_prepared_assignment(target, func, ctx);
    } else {
        assign_pattern(pat, func, ctx);
    }
}

/// DestructuringAssignmentEvaluation. Unlike binding initialization, leaf
/// members are real References and array patterns consume the iterator
/// protocol instead of indexing an array-like value.
fn assign_pattern(pat: &Pat, func: &mut BytecodeFunction, ctx: &mut CompilerCtx) {
    match pat {
        Pat::Ident { .. } | Pat::Member(_) | Pat::Assignment { .. } | Pat::Rest { .. } => {
            let prepared = prepare_assignment_target(pat, func, ctx);
            assign_prepared_value(pat, prepared, func, ctx);
        }
        Pat::Object { properties, .. } => {
            let source = fresh_temp(func, "destr-object");
            func.emit(Instruction::new(Opcode::StaLocal, source));
            for property in properties {
                match property {
                    js_syntax::ast::pat::ObjectPatProp::KeyValue { key, value, .. } => {
                        // PropertyName evaluation precedes target evaluation.
                        let property_key = fresh_temp(func, "destr-property-key");
                        compile_prop_key_push(key, false, func, ctx);
                        func.emit(Instruction::new(Opcode::StaLocal, property_key));
                        let prepared = prepare_assignment_target(value, func, ctx);
                        func.emit(Instruction::new(Opcode::LdaLocal, source));
                        func.emit(Instruction::new(Opcode::LdaLocal, property_key));
                        func.emit_bare(Opcode::GetProp);
                        assign_prepared_value(value, prepared, func, ctx);
                    }
                    js_syntax::ast::pat::ObjectPatProp::Rest { arg, .. } => {
                        // CopyDataProperties exclusion is implemented in the
                        // next descriptor batch; retain assignment semantics.
                        let prepared = prepare_assignment_target(arg, func, ctx);
                        func.emit(Instruction::new(Opcode::LdaLocal, source));
                        assign_prepared_value(arg, prepared, func, ctx);
                    }
                }
            }
        }
        Pat::Array { elements, .. } => {
            let iterator = fresh_temp(func, "destr-iterator");
            func.emit_bare(Opcode::GetIterator);
            func.emit(Instruction::new(Opcode::StaLocal, iterator));
            for element in elements.iter().flatten() {
                let inner = match element {
                    js_syntax::ast::pat::ArrayPatElement::Hole(_) => {
                        func.emit(Instruction::new(Opcode::LdaLocal, iterator));
                        func.emit_bare(Opcode::IterNext);
                        func.emit_bare(Opcode::Pop);
                        continue;
                    }
                    js_syntax::ast::pat::ArrayPatElement::Pat(inner) => inner,
                };
                if let Pat::Rest { arg, .. } = inner {
                    // The existing bytecode has no iterator-to-list opcode yet;
                    // preserve a correctly typed empty result until that opcode
                    // is introduced with spread call/new.
                    let prepared = prepare_assignment_target(arg, func, ctx);
                    func.emit(Instruction::new(Opcode::NewArray, 0));
                    assign_prepared_value(arg, prepared, func, ctx);
                    continue;
                }
                let prepared = prepare_assignment_target(inner, func, ctx);
                func.emit(Instruction::new(Opcode::LdaLocal, iterator));
                func.emit_bare(Opcode::IterNext);
                func.emit_bare(Opcode::Dup);
                let done_key = ctx.constants.intern_str("done");
                func.emit(Instruction::new(Opcode::LdaConst, done_key));
                func.emit_bare(Opcode::GetProp);
                let has_value = emit_placeholder(func, Opcode::JumpIfFalse);
                func.emit_bare(Opcode::Pop);
                func.emit_bare(Opcode::LdaUndefined);
                let joined = emit_placeholder(func, Opcode::Jump);
                patch(func, has_value, func.here());
                let value_key = ctx.constants.intern_str("value");
                func.emit(Instruction::new(Opcode::LdaConst, value_key));
                func.emit_bare(Opcode::GetProp);
                patch(func, joined, func.here());
                assign_prepared_value(inner, prepared, func, ctx);
            }
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
        Stmt::Labeled { body, .. } | Stmt::With { body, .. } => collect_stmt_bindings(body, out),
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
        Decl::Import { spec, .. } => match spec {
            ImportSpec::Bare { .. } => {}
            ImportSpec::Namespace { ns, .. } => out.push(ns.clone()),
            ImportSpec::Named { items, .. } => {
                out.extend(items.iter().map(|item| item.local.clone()));
            }
            ImportSpec::Default {
                local,
                namespace,
                named,
                ..
            } => {
                out.push(local.clone());
                out.extend(namespace.iter().cloned());
                out.extend(named.iter().map(|item| item.local.clone()));
            }
        },
        Decl::Export { spec, .. } => match spec {
            ExportSpec::Decl(inner) => collect_decl_bindings(inner, out),
            ExportSpec::Default(_) => out.push(crate::module::DEFAULT_EXPORT_LOCAL.to_string()),
            ExportSpec::DefaultDecl(inner) => match inner.as_ref() {
                Decl::Function(function) if function.name.is_none() => {
                    out.push(crate::module::DEFAULT_EXPORT_LOCAL.to_string());
                }
                Decl::Class(class) if class.name.is_none() => {
                    out.push(crate::module::DEFAULT_EXPORT_LOCAL.to_string());
                }
                inner => collect_decl_bindings(inner, out),
            },
            ExportSpec::Named { .. } | ExportSpec::All { .. } | ExportSpec::ReExport { .. } => {}
        },
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
    let reference = ctx.resolve_var(name);
    if ctx.uses_dynamic_name(&reference) {
        let idx = ctx.constants.intern_str(name);
        func.emit(Instruction::new(Opcode::GetName, idx));
        return;
    }
    match reference {
        VarRef::Local(slot) => func.emit(Instruction::new(Opcode::LdaLocal, slot)),
        VarRef::Upvalue(idx) => func.emit(Instruction::new(Opcode::LdaUpvalue, idx)),
        VarRef::Global => {
            let idx = ctx.constants.intern_str(name);
            func.emit(Instruction::new(Opcode::GetGlobal, idx));
        }
    }
}

fn store_ident(name: &str, func: &mut BytecodeFunction, ctx: &mut CompilerCtx) {
    let reference = ctx.resolve_var(name);
    if ctx.uses_dynamic_name(&reference) {
        let idx = ctx.constants.intern_str(name);
        func.emit(Instruction::new(Opcode::SetName, idx));
        return;
    }
    match reference {
        VarRef::Local(slot) => func.emit(Instruction::new(Opcode::StaLocal, slot)),
        VarRef::Upvalue(idx) => func.emit(Instruction::new(Opcode::StaUpvalue, idx)),
        VarRef::Global => {
            let idx = ctx.constants.intern_str(name);
            func.emit(Instruction::new(Opcode::SetGlobal, idx));
        }
    }
}

/// BindingInitialization targets the binding created by the declaration; it
/// does not perform object Environment Record name resolution.
fn store_binding_ident(name: &str, func: &mut BytecodeFunction, ctx: &mut CompilerCtx) {
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
            let target = prepare_assignment_target(&Pat::Member(m.clone()), func, ctx)
                .expect("member update target must prepare");
            load_prepared_assignment(&target, func, ctx);
            if prefix {
                emit_delta(func);
                func.emit_bare(Opcode::Dup);
                put_prepared_assignment(target, func, ctx);
            } else {
                func.emit_bare(Opcode::Dup);
                emit_delta(func);
                put_prepared_assignment(target, func, ctx);
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
    is_module: bool,
    module_function_initializers: Vec<(u16, u32)>,
    dynamic_import_requests: Vec<String>,
    errors: Vec<Diagnostic>,
    /// Stack of enclosing loops/switches for `break`/`continue`. Each frame
    /// records forward-jump placeholders to patch at loop exit: `breaks` → after
    /// the loop, `continues` → the update/test section. Switches use `breaks`
    /// only (`continues` stays empty and a `continue` inside a bare switch is
    /// rejected by the caller).
    loops: Vec<LoopFrame>,
    /// Non-iteration labelled statements. Their only runtime control target is
    /// `break label`; iteration labels live directly on `LoopFrame` so both
    /// labelled break and labelled continue resolve to the same statement.
    labels: Vec<LabelFrame>,
    /// Number of syntactically enclosing `with` statements and the depth at
    /// which each compiled function began. Together these distinguish a
    /// function's own locals from outer names resolved through captured object
    /// Environment Records.
    with_depth: usize,
    function_with_bases: Vec<usize>,
    /// Lexical scope stack, one entry per *function* being compiled (scopes[0]
    /// is `<main>`). Drives closure upvalue resolution.
    scopes: Vec<Scope>,
    /// Lexically nested class private environments. Values identify the class
    /// definition whose brand owns a private spelling.
    private_scopes: Vec<std::collections::HashMap<String, u32>>,
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
    labels: Vec<String>,
    is_iteration: bool,
}

struct LabelFrame {
    labels: Vec<String>,
    breaks: Vec<u16>,
}

/// Retain the complete visible lexical chain on function closures. Direct eval
/// can reference bindings that do not occur syntactically in the containing
/// function, so capture-only-when-mentioned is insufficient for ECMAScript.
fn capture_visible_environment(ctx: &mut CompilerCtx) {
    if ctx.scopes.len() < 2 {
        return;
    }
    let current = ctx.scopes.len() - 1;
    let mut names = Vec::new();
    for scope in &ctx.scopes[..current] {
        names.extend(scope.locals.keys().cloned());
        names.extend(scope.upvalues.iter().map(|binding| binding.name.clone()));
    }
    names.sort();
    names.dedup();
    for name in names {
        if !ctx.scopes[current].locals.contains_key(&name) {
            let _ = ctx.resolve_var(&name);
        }
    }
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

    fn private_name_constant(&mut self, name: &str, span: js_syntax::Span) -> u16 {
        let class_id = self
            .private_scopes
            .iter()
            .rev()
            .find_map(|scope| scope.get(name).copied());
        match class_id {
            Some(class_id) => self.constants.intern_str(format!("{class_id}\0{name}")),
            None => {
                self.errors.push(Diagnostic::error(
                    span,
                    format!("private name `#{name}` has no enclosing class brand"),
                ));
                self.constants.intern_str(format!("invalid\0{name}"))
            }
        }
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

    fn uses_dynamic_name(&self, reference: &VarRef) -> bool {
        if self.with_depth == 0 {
            return false;
        }
        let function_base = self.function_with_bases.last().copied().unwrap_or(0);
        self.with_depth > function_base || !matches!(reference, VarRef::Local(_))
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

    fn push_loop(&mut self, labels: &[String], is_iteration: bool) {
        self.loops.push(LoopFrame {
            labels: labels.to_vec(),
            is_iteration,
            ..LoopFrame::default()
        });
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
    fn push_label(&mut self, labels: Vec<String>) {
        self.labels.push(LabelFrame {
            labels,
            breaks: Vec::new(),
        });
    }

    fn pop_label(&mut self, func: &mut BytecodeFunction, break_target: u16) {
        if let Some(frame) = self.labels.pop() {
            for at in frame.breaks {
                patch(func, at, break_target);
            }
        }
    }

    fn emit_break(&mut self, func: &mut BytecodeFunction, label: Option<&str>) -> bool {
        if let Some(label) = label {
            if let Some(frame) = self
                .loops
                .iter_mut()
                .rev()
                .find(|frame| frame.labels.iter().any(|candidate| candidate == label))
            {
                frame.breaks.push(emit_placeholder(func, Opcode::Jump));
                return true;
            }
            if let Some(frame) = self
                .labels
                .iter_mut()
                .rev()
                .find(|frame| frame.labels.iter().any(|candidate| candidate == label))
            {
                frame.breaks.push(emit_placeholder(func, Opcode::Jump));
                return true;
            }
            return false;
        }

        let Some(frame) = self.loops.last_mut() else {
            return false;
        };
        frame.breaks.push(emit_placeholder(func, Opcode::Jump));
        true
    }
    fn emit_continue(&mut self, func: &mut BytecodeFunction, label: Option<&str>) -> bool {
        let frame = match label {
            Some(label) => self.loops.iter_mut().rev().find(|frame| {
                frame.is_iteration && frame.labels.iter().any(|candidate| candidate == label)
            }),
            None => self.loops.iter_mut().rev().find(|frame| frame.is_iteration),
        };
        let Some(frame) = frame else {
            return false;
        };
        frame.continues.push(emit_placeholder(func, Opcode::Jump));
        true
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
