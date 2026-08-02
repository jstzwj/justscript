//! Post-parse semantic checks: ECMAScript *early errors* and strict-mode rules.
//!
//! The recursive-descent parser only enforces *syntactic* well-formedness. Many
//! spec constraints are *early errors* that need a semantic walk once the AST is
//! in hand — chiefly rules that depend on **strict mode**, which is itself a
//! function-local property derived from directive prologues and the containing
//! construct (modules, classes, generators/async, arrows are strict).
//!
//! [`check`] runs after a successful parse and returns any diagnostics. The
//! public [`Parser::parse`](crate::Parser::parse) merges them with parse-time
//! errors, so the test262 runner sees them as parse failures.

use js_diagnostics::Diagnostic;
use js_syntax::ast::expr::{
    ArrowBody, AssignTarget, ClassMemberKind, Expr, MemberExpr, MemberProp,
};
use js_syntax::ast::lit::Lit;
use js_syntax::ast::op::UnaryOp;
use js_syntax::ast::pat::{ArrayPatElement, ObjectPatProp, Pat};
use js_syntax::ast::stmt::{Decl, Stmt};
use js_syntax::ast::{ClassDecl, FunctionDecl, Program, ProgramItem, ProgramKind};
use js_syntax::Span;

use crate::static_semantics::{
    bound_names, contains_arguments, contains_use_strict, import_local_names,
    is_simple_parameter_list, module_static_semantics, program_contains_use_strict,
    statement_list_lexically_declared_names, statement_list_var_declared_names,
    statements_contain_arguments, switch_lexically_declared_names, switch_var_declared_names,
    var_declared_names,
};

/// Syntactic context inherited by a direct eval from its caller.
#[derive(Clone, Debug, Default)]
pub struct EvalContext {
    pub strict: bool,
    pub private_names: std::collections::HashSet<String>,
    pub allow_super_property: bool,
    pub allow_super_call: bool,
    pub allow_new_target: bool,
    pub reject_arguments: bool,
}

/// Run all early-error checks against a parsed [`Program`].
pub fn check(program: &Program) -> Vec<Diagnostic> {
    check_with_eval_context(program, None)
}

/// Run early errors for eval code with the syntactic context inherited from a
/// direct caller. Indirect eval passes no context and therefore uses Script
/// rules exactly like [`check`].
pub fn check_eval(program: &Program, context: &EvalContext) -> Vec<Diagnostic> {
    check_with_eval_context(program, Some(context))
}

fn check_with_eval_context(
    program: &Program,
    eval_context: Option<&EvalContext>,
) -> Vec<Diagnostic> {
    let is_module = program.kind == ProgramKind::Module;
    let inherited_private = eval_context
        .filter(|context| !context.private_names.is_empty())
        .map(|context| context.private_names.clone())
        .into_iter()
        .collect();
    let mut c = Checker {
        errors: Vec::new(),
        is_module,
        strict: is_module
            || eval_context.is_some_and(|context| context.strict)
            || program_contains_use_strict(&program.body),
        scopes: vec![Scope::new(true)],
        // Modules allow top-level `await`; classic scripts do not. `yield` is
        // never valid at the top level.
        await_ctx: vec![is_module],
        yield_ctx: vec![false],
        super_prop: vec![eval_context.is_some_and(|context| context.allow_super_property)],
        super_call: vec![eval_context.is_some_and(|context| context.allow_super_call)],
        new_target_allowed: eval_context.is_some_and(|context| context.allow_new_target),
        private_env: inherited_private,
        static_block_depth: 0,
        labels: Vec::new(),
        breakable_depth: 0,
        iteration_depth: 0,
    };
    if is_module {
        c.check_module_body(&program.body, program.span);
    }
    for item in &program.body {
        if !is_module && program_item_is_using_declaration(item) {
            c.err(
                program_item_span(item),
                "using declarations are not allowed at the top level of a script",
            );
        }
        c.check_item(item);
    }
    if eval_context.is_some_and(|context| context.reject_arguments) {
        for item in &program.body {
            if let ProgramItem::Stmt(statement) = item {
                if statements_contain_arguments(std::slice::from_ref(statement)) {
                    c.err(
                        statement.span(),
                        "`arguments` is not allowed in direct eval inside a class initializer",
                    );
                }
            }
        }
    }
    c.errors
}

/// One lexical scope: block, function-body, catch, or for-loop. `lexical`
/// holds `let`/`const`/`class` (and function-scope params); `vars` holds
/// hoisted `var`/`function` declarations.
#[derive(Default)]
struct Scope {
    lexical: std::collections::HashMap<String, LexicalBinding>,
    vars: std::collections::HashSet<String>,
    is_function: bool,
}

#[derive(Clone, Copy)]
struct LexicalBinding {
    ordinary_function: bool,
}

impl Scope {
    fn new(is_function: bool) -> Scope {
        Scope {
            lexical: Default::default(),
            vars: Default::default(),
            is_function,
        }
    }
}

struct Checker {
    errors: Vec<Diagnostic>,
    /// The syntactic goal used for the whole parse. Unlike strictness, this is
    /// not inherited or changed by entering functions/classes.
    is_module: bool,
    /// Whether the *current* lexical context is strict mode.
    strict: bool,
    /// Lexical scope stack; scopes[0] is the script/module top level.
    scopes: Vec<Scope>,
    /// Effective `await`-allowed stack (arrows inherit; module top-level true).
    await_ctx: Vec<bool>,
    /// Effective `yield`-allowed stack (only generator functions set true).
    yield_ctx: Vec<bool>,
    /// `super.x` allowed here (directly in a class method/ctor, or arrow nested).
    super_prop: Vec<bool>,
    /// `super()` call allowed here (in a derived class's constructor).
    super_call: Vec<bool>,
    /// `new.target` is valid in function code and class static blocks. Arrow
    /// functions inherit this context; scripts and modules do not.
    new_target_allowed: bool,
    /// Lexically enclosing class private environments. The current class is
    /// last; nested classes can also see names from their enclosing classes.
    private_env: Vec<std::collections::HashSet<String>>,
    /// Non-zero while walking a class static block. Nested functions reset it:
    /// the specification's ContainsAwait query does not cross function bodies.
    static_block_depth: usize,
    /// Active labels and whether each ultimately labels an iteration statement.
    labels: Vec<(String, bool)>,
    /// Unlabelled `break` and `continue` target depths.
    breakable_depth: usize,
    iteration_depth: usize,
}

impl Checker {
    fn err(&mut self, span: Span, msg: impl Into<String>) {
        self.errors.push(Diagnostic::error(span, msg));
    }

    fn check_module_body(&mut self, body: &[ProgramItem], span: Span) {
        let semantics = module_static_semantics(body);
        let mut declared = semantics.var_declared_names.clone();
        let mut seen_lexical = std::collections::HashSet::new();
        for name in &semantics.lexically_declared_names {
            if !seen_lexical.insert(name.clone()) {
                self.err(
                    span,
                    format!("module has multiple lexical declarations for `{name}`"),
                );
            }
            if semantics.var_declared_names.contains(name) {
                self.err(
                    span,
                    format!("module lexical declaration `{name}` conflicts with a var declaration"),
                );
            }
            declared.insert(name.clone());
        }

        let mut seen_exports = std::collections::HashSet::new();
        for name in &semantics.exported_names {
            if !seen_exports.insert(name.clone()) {
                self.err(span, format!("module exports `{name}` more than once"));
            }
        }
        for name in &semantics.exported_bindings {
            if !declared.contains(name) {
                self.err(
                    span,
                    format!("exported binding `{name}` is not declared by this module"),
                );
            }
        }
        for name in &semantics.imported_local_names {
            self.check_strict_binding_name(name, span);
        }
        for name in &semantics.invalid_local_export_names {
            self.err(
                span,
                format!("string export name `{name}` cannot reference a local binding"),
            );
        }
        for (attribute_span, key) in &semantics.duplicate_attribute_keys {
            self.err(
                *attribute_span,
                format!("import attribute key `{key}` occurs more than once"),
            );
        }
    }

    // ---- scope helpers --------------------------------------------------

    fn enter(&mut self, is_function: bool) {
        self.scopes.push(Scope::new(is_function));
    }

    fn leave(&mut self) {
        self.scopes.pop();
    }

    fn take_control_context(&mut self) -> (Vec<(String, bool)>, usize, usize) {
        (
            std::mem::take(&mut self.labels),
            std::mem::replace(&mut self.breakable_depth, 0),
            std::mem::replace(&mut self.iteration_depth, 0),
        )
    }

    fn restore_control_context(&mut self, context: (Vec<(String, bool)>, usize, usize)) {
        (self.labels, self.breakable_depth, self.iteration_depth) = context;
    }

    /// Declare a lexical binding (`let`/`const`/`class`, or a param) in the
    /// current scope. Errors on a same-scope collision with another lexical or
    /// a `var`/`function` binding.
    fn declare_lexical(&mut self, name: &str, span: Span) {
        self.declare_lexical_kind(name, span, false);
    }

    fn declare_block_function(&mut self, name: &str, span: Span, ordinary: bool) {
        self.declare_lexical_kind(name, span, ordinary);
    }

    fn declare_lexical_kind(&mut self, name: &str, span: Span, ordinary_function: bool) {
        let scope = self.scopes.last_mut().unwrap();
        if let Some(previous) = scope.lexical.get(name) {
            let annex_b_pair = !self.strict && previous.ordinary_function && ordinary_function;
            if !annex_b_pair {
                self.err(
                    span,
                    format!("identifier `{}` has already been declared", name),
                );
                return;
            }
        }
        if scope.vars.contains(name) {
            self.err(
                span,
                format!("identifier `{}` has already been declared", name),
            );
            return;
        }
        scope
            .lexical
            .insert(name.to_string(), LexicalBinding { ordinary_function });
    }

    /// Declare a hoisted `var`/`function` binding in the nearest function
    /// scope. Errors if a lexical binding of the same name exists in that
    /// function scope.
    fn declare_var(&mut self, name: &str, span: Span) {
        // `var`/`function` hoist to the nearest function scope.
        let fn_idx = self.scopes.iter().rposition(|s| s.is_function).unwrap_or(0);
        if self.scopes[fn_idx..]
            .iter()
            .any(|scope| scope.lexical.contains_key(name))
        {
            self.err(
                span,
                format!("identifier `{}` has already been declared", name),
            );
            return;
        }
        self.scopes[fn_idx].vars.insert(name.to_string());
    }

    fn check_item(&mut self, item: &ProgramItem) {
        match item {
            ProgramItem::Stmt(s) => self.check_stmt(s),
            ProgramItem::Decl(d) => {
                if !self.is_module && matches!(d, Decl::Import { .. } | Decl::Export { .. }) {
                    self.err(
                        d.span(),
                        "import/export declarations are only valid in module code",
                    );
                }
                self.check_decl(d);
            }
        }
    }

    /// Validate the body of an unbraced `if`/`for`/`while`/`do` clause, then
    /// recurse. A non-block single-statement body may not be a lexical
    /// (`let`/`const`), class, or function declaration — those require a block.
    fn check_unbraced_body(&mut self, body: &Stmt) {
        if is_labelled_function(body) {
            self.err(
                body.span(),
                "a labelled function may not be the body of this statement",
            );
        }
        // A non-block single-statement body may not be a lexical
        // (`let`/`const`/`using`), class, or function declaration — those
        // require a block. (`var` is permitted.)
        let bad = if let Stmt::Decl(d) = body {
            matches!(d.as_ref(), Decl::Class(_) | Decl::Function(_))
                || matches!(d.as_ref(), Decl::Var { kind, .. } if matches!(
                    kind,
                    js_syntax::ast::stmt::VarKind::Let
                        | js_syntax::ast::stmt::VarKind::Const
                        | js_syntax::ast::stmt::VarKind::Using
                        | js_syntax::ast::stmt::VarKind::AwaitUsing
                ))
        } else {
            false
        };
        if bad {
            self.err(body.span(), "lexical/class/function declaration cannot be the body of an unbraced statement; use a block `{ ... }`");
        }
        self.check_stmt(body);
    }

    fn check_stmt(&mut self, stmt: &Stmt) {
        match stmt {
            Stmt::Block { body, .. } => {
                self.check_statement_list_names(body, stmt.span());
                self.enter(false);
                for s in body {
                    self.check_stmt(s);
                }
                self.leave();
            }
            Stmt::Expr { expr, .. } => self.check_expr(expr),
            Stmt::Decl(d) => self.check_decl(d),
            Stmt::If {
                test, cons, alt, ..
            } => {
                self.check_expr(test);
                self.check_unbraced_body(cons);
                if let Some(a) = alt {
                    self.check_unbraced_body(a);
                }
            }
            Stmt::While { test, body, .. } | Stmt::DoWhile { test, body, .. } => {
                self.check_expr(test);
                self.breakable_depth += 1;
                self.iteration_depth += 1;
                self.check_unbraced_body(body);
                self.iteration_depth -= 1;
                self.breakable_depth -= 1;
            }
            Stmt::For {
                init,
                test,
                update,
                body,
                ..
            } => {
                // The init's `let`/`const` bindings live in a scope wrapping the loop.
                self.enter(false);
                if let Some(init) = init {
                    match init {
                        js_syntax::ast::stmt::ForInit::Var(d) => self.check_decl(d),
                        js_syntax::ast::stmt::ForInit::Expr(e) => self.check_expr(e),
                    }
                }
                if let Some(t) = test {
                    self.check_expr(t);
                }
                if let Some(u) = update {
                    self.check_expr(u);
                }
                self.breakable_depth += 1;
                self.iteration_depth += 1;
                self.check_unbraced_body(body);
                self.iteration_depth -= 1;
                self.breakable_depth -= 1;
                self.leave();
            }
            Stmt::ForIn {
                left, right, body, ..
            }
            | Stmt::ForOf {
                left, right, body, ..
            } => {
                let is_for_in = matches!(stmt, Stmt::ForIn { .. });
                self.enter(false);
                if let js_syntax::ast::stmt::ForTarget::Var(d) = left {
                    // `using`/`await using` are not permitted as `for...in` targets.
                    if is_for_in {
                        if let Decl::Var { kind, span, .. } = d.as_ref() {
                            if matches!(
                                kind,
                                js_syntax::ast::stmt::VarKind::Using
                                    | js_syntax::ast::stmt::VarKind::AwaitUsing
                            ) {
                                self.err(*span, "`using` is not allowed in a `for...in` statement");
                            }
                        }
                    }
                    if let Decl::Var {
                        kind, declarations, ..
                    } = d.as_ref()
                    {
                        if !matches!(kind, js_syntax::ast::stmt::VarKind::Var) {
                            let names: Vec<String> = declarations
                                .iter()
                                .flat_map(|declaration| bound_names(&declaration.name))
                                .collect();
                            if names.iter().any(|name| name == "let") {
                                self.err(d.span(), "a for declaration may not bind `let`");
                            }
                            let body_vars = var_declared_names(body);
                            for name in names {
                                if body_vars.contains(&name) {
                                    self.err(
                                        d.span(),
                                        format!(
                                            "for declaration `{name}` conflicts with a var declaration in its body"
                                        ),
                                    );
                                }
                            }
                        }
                    }
                    self.check_decl_opts(d, true);
                } else if let js_syntax::ast::stmt::ForTarget::Pat(pattern) = left {
                    self.check_binding_pat(pattern);
                    self.check_assignment_pattern(pattern);
                }
                self.check_expr(right);
                self.breakable_depth += 1;
                self.iteration_depth += 1;
                self.check_unbraced_body(body);
                self.iteration_depth -= 1;
                self.breakable_depth -= 1;
                self.leave();
            }
            Stmt::Switch { disc, cases, .. } => {
                self.check_expr(disc);
                for declaration in cases
                    .iter()
                    .flat_map(|case| &case.body)
                    .filter(|statement| stmt_is_using_declaration(statement))
                {
                    self.err(
                        declaration.span(),
                        "a using declaration may not appear directly in a switch clause",
                    );
                }
                let lexical_names = switch_lexically_declared_names(cases);
                let var_names = switch_var_declared_names(cases);
                let mut seen_lexical = std::collections::HashMap::new();
                for declaration in lexical_names {
                    if let Some(previous_was_ordinary_function) =
                        seen_lexical.insert(declaration.name.clone(), declaration.ordinary_function)
                    {
                        let annex_b_pair = !self.strict
                            && previous_was_ordinary_function
                            && declaration.ordinary_function;
                        if !annex_b_pair {
                            self.err(
                                stmt.span(),
                                format!(
                                    "identifier `{}` has multiple lexical declarations in switch",
                                    declaration.name
                                ),
                            );
                        }
                    }
                    if var_names.contains(&declaration.name) {
                        self.err(
                            stmt.span(),
                            format!(
                                "lexical declaration `{}` conflicts with a var declaration in switch",
                                declaration.name
                            ),
                        );
                    }
                }
                // A switch is one block scope shared by all case bodies.
                self.enter(false);
                self.breakable_depth += 1;
                let mut default_count = 0;
                for c in cases {
                    if let Some(t) = &c.test {
                        self.check_expr(t);
                    } else {
                        default_count += 1;
                    }
                    for s in &c.body {
                        self.check_stmt(s);
                    }
                }
                self.breakable_depth -= 1;
                self.leave();
                if default_count > 1 {
                    self.err(stmt.span(), "switch may have at most one `default` clause");
                }
            }
            Stmt::Throw { arg, .. } => self.check_expr(arg),
            Stmt::Try {
                block,
                handler,
                finalizer,
                ..
            } => {
                self.check_statement_list_names(&block.body, block.span);
                self.enter(false);
                for s in &block.body {
                    self.check_stmt(s);
                }
                self.leave();
                if let Some(h) = handler {
                    // The catch parameter shares a scope with the catch body.
                    self.check_statement_list_names(&h.body, h.span);
                    self.enter(false);
                    if let Some(p) = &h.param {
                        let names = bound_names(p);
                        for n in &names {
                            self.declare_lexical(n, p.span());
                        }
                        self.check_binding_pat(p);
                    }
                    for s in &h.body {
                        self.check_stmt(s);
                    }
                    self.leave();
                }
                if let Some(f) = finalizer {
                    self.check_statement_list_names(f, stmt.span());
                    self.enter(false);
                    for s in f {
                        self.check_stmt(s);
                    }
                    self.leave();
                }
            }
            Stmt::Labeled {
                label, body, span, ..
            } => {
                if self.labels.iter().any(|(active, _)| active == label) {
                    self.err(*span, format!("duplicate label `{label}`"));
                }
                if label == "yield" && (self.strict || *self.yield_ctx.last().unwrap()) {
                    self.err(*span, "`yield` may not be used as a label in this context");
                }
                if label == "await"
                    && (*self.await_ctx.last().unwrap() || self.static_block_depth > 0)
                {
                    self.err(*span, "`await` may not be used as a label in this context");
                }
                self.labels
                    .push((label.clone(), label_targets_iteration(body)));
                if labelled_body_declaration_is_invalid(body, self.strict) {
                    self.err(
                        body.span(),
                        "this declaration may not be the body of a labelled statement",
                    );
                }
                self.check_stmt(body);
                self.labels.pop();
            }
            Stmt::With { obj, body, .. } => {
                self.check_expr(obj);
                if self.strict {
                    self.err(stmt.span(), "`with` is not allowed in strict mode");
                }
                self.check_unbraced_body(body);
            }
            Stmt::Return { arg, .. } => {
                if self.static_block_depth > 0 {
                    self.err(
                        stmt.span(),
                        "a return statement is not allowed in a class static block",
                    );
                }
                if let Some(a) = arg {
                    self.check_expr(a);
                }
            }
            Stmt::Break { label, span } => match label {
                Some(label) if !self.labels.iter().any(|(active, _)| active == label) => {
                    self.err(*span, format!("undefined break target `{label}`"));
                }
                None if self.breakable_depth == 0 => {
                    self.err(*span, "break statement is not inside a loop or switch");
                }
                _ => {}
            },
            Stmt::Continue { label, span } => match label {
                Some(label)
                    if !self
                        .labels
                        .iter()
                        .any(|(active, iteration)| active == label && *iteration) =>
                {
                    self.err(*span, format!("invalid continue target `{label}`"));
                }
                None if self.iteration_depth == 0 => {
                    self.err(*span, "continue statement is not inside a loop");
                }
                _ => {}
            },
            Stmt::Empty(_) | Stmt::Debugger(_) => {}
        }
    }

    fn check_decl(&mut self, decl: &Decl) {
        self.check_decl_opts(decl, false)
    }

    fn check_decl_opts(&mut self, decl: &Decl, is_iteration_target: bool) {
        match decl {
            Decl::Var {
                kind, declarations, ..
            } => {
                let is_lexical = matches!(
                    kind,
                    js_syntax::ast::stmt::VarKind::Let
                        | js_syntax::ast::stmt::VarKind::Const
                        | js_syntax::ast::stmt::VarKind::Using
                        | js_syntax::ast::stmt::VarKind::AwaitUsing
                );
                if is_lexical
                    && declarations
                        .iter()
                        .flat_map(|declaration| bound_names(&declaration.name))
                        .any(|name| name == "let")
                {
                    self.err(
                        decl.span(),
                        "a lexical declaration may not bind the name `let`",
                    );
                }
                for d in declarations {
                    // Declare the binding names with scope-aware conflict checks.
                    let names = bound_names(&d.name);
                    for n in &names {
                        if is_lexical {
                            self.declare_lexical(n, d.span);
                        } else {
                            self.declare_var(n, d.span);
                        }
                    }
                    // Per-binding strict checks + structural rules.
                    self.check_binding_pat(&d.name);
                    if let Some(init) = &d.init {
                        self.check_expr(init);
                    } else {
                        // `const` and `using`/`await using` need an initializer
                        // — except when the declaration is a `for...of`/`for...in`
                        // iteration target, where the binding is fed by iteration.
                        let needs_init = !is_iteration_target
                            && matches!(
                                kind,
                                js_syntax::ast::stmt::VarKind::Const
                                    | js_syntax::ast::stmt::VarKind::Using
                                    | js_syntax::ast::stmt::VarKind::AwaitUsing
                            );
                        if needs_init {
                            self.err(d.span, "this declaration must have an initializer");
                        }
                    }
                }
            }
            Decl::Function(f) => {
                if let Some(name) = &f.name {
                    if self.is_module && self.scopes.len() == 1 {
                        self.declare_lexical(name, f.span);
                    } else if self.scopes.last().is_some_and(|scope| scope.is_function) {
                        self.declare_var(name, f.span);
                    } else {
                        self.declare_block_function(name, f.span, !f.is_async && !f.is_generator);
                    }
                    // A declaration's BindingIdentifier belongs to the
                    // surrounding scope, so it remains inside a class static
                    // block even though the function's parameters and body do
                    // not. Named function expressions are handled wholly by
                    // `check_function` and therefore sit beyond the boundary.
                    if self.static_block_depth > 0 && name == "await" {
                        self.err(
                            f.span,
                            "`await` may not be used as a function declaration binding inside a class static block",
                        );
                    }
                }
                self.check_function(f, false, false, false, false, false);
            }
            Decl::Class(c) => {
                if let Some(name) = &c.name {
                    self.declare_lexical(name, c.span);
                }
                self.check_class(c);
            }
            Decl::Import { spec, .. } => {
                for name in import_local_names(spec) {
                    self.declare_lexical(&name, decl.span());
                    self.check_strict_binding_name(&name, decl.span());
                }
            }
            Decl::Export { spec, .. } => match spec {
                js_syntax::ast::stmt::ExportSpec::Default(expr) => self.check_expr(expr),
                js_syntax::ast::stmt::ExportSpec::DefaultDecl(decl)
                | js_syntax::ast::stmt::ExportSpec::Decl(decl) => self.check_decl(decl),
                js_syntax::ast::stmt::ExportSpec::Named { .. }
                | js_syntax::ast::stmt::ExportSpec::All { .. }
                | js_syntax::ast::stmt::ExportSpec::ReExport { .. } => {}
            },
        }
    }

    /// Apply the Block/StatementList Early Errors as set operations. This is
    /// intentionally a pre-pass: `VarDeclaredNames` recurses into nested
    /// statements, while `LexicallyDeclaredNames` only contains declarations
    /// directly owned by this StatementList.
    fn check_statement_list_names(&mut self, statements: &[Stmt], span: Span) {
        let lexical = statement_list_lexically_declared_names(statements);
        let vars = statement_list_var_declared_names(statements);
        let mut seen = std::collections::HashMap::new();
        for declaration in lexical {
            if let Some(previous_ordinary) =
                seen.insert(declaration.name.clone(), declaration.ordinary_function)
            {
                let annex_b_pair =
                    !self.strict && previous_ordinary && declaration.ordinary_function;
                if !annex_b_pair {
                    self.err(
                        span,
                        format!(
                            "identifier `{}` has multiple lexical declarations in block",
                            declaration.name
                        ),
                    );
                }
            }
            if vars.contains(&declaration.name) {
                self.err(
                    span,
                    format!(
                        "lexical declaration `{}` conflicts with a var declaration in block",
                        declaration.name
                    ),
                );
            }
        }
    }

    fn check_function(
        &mut self,
        f: &FunctionDecl,
        force_strict: bool,
        super_prop_ok: bool,
        super_call_ok: bool,
        name_is_property: bool,
        unique_params: bool,
    ) {
        // Strictness: inherited, forced (class member/generator/async), or own
        // directive prologue.
        let has_use_strict = contains_use_strict(&f.body);
        let body_strict = self.strict || force_strict || has_use_strict;
        // A FunctionDefinition is a boundary for the static block's
        // ContainsAwait query. That boundary includes the function expression's
        // BindingIdentifier, parameters, and body, not only its body statements.
        let enclosing_static_block_depth = self.static_block_depth;
        self.static_block_depth = 0;
        let enclosing_new_target = std::mem::replace(&mut self.new_target_allowed, true);
        // Function name (strict): eval/arguments/FRW restrictions. A *method*
        // name is a property name (IdentifierName), not a BindingIdentifier, so
        // it is exempt — `{ eval(){} }` / `class C { arguments(){} }` are fine.
        if body_strict && !name_is_property {
            if let Some(name) = &f.name {
                self.check_strict_binding_name(name, f.span);
            }
        }
        let enclosing_control = self.take_control_context();
        // Parameters.
        let non_simple = !is_simple_parameter_list(&f.params);
        if has_use_strict && non_simple {
            self.err(
                f.span,
                "a function with a non-simple parameter list may not contain a `use strict` directive",
            );
        }
        // Enter a function scope so params + body lexical declarations interact
        // (e.g. a `let` in the body clashing with a parameter is an error).
        self.enter(true);
        self.declare_params(&f.params);
        self.check_param_list(&f.params, non_simple, body_strict, unique_params);
        self.check_parameter_initializers(&f.params, body_strict, super_prop_ok);
        // A regular (non-arrow) function establishes its own await/yield and
        // resets super context (super is not valid inside nested non-arrow fns).
        self.await_ctx.push(f.is_async);
        self.yield_ctx.push(f.is_generator);
        self.super_prop.push(super_prop_ok);
        self.super_call.push(super_call_ok);
        // Recurse into the body under the function's own strictness.
        let saved = self.strict;
        self.strict = body_strict;
        for s in &f.body {
            self.check_stmt(s);
        }
        self.strict = saved;
        self.await_ctx.pop();
        self.yield_ctx.pop();
        self.super_prop.pop();
        self.super_call.pop();
        self.leave();
        self.restore_control_context(enclosing_control);
        self.static_block_depth = enclosing_static_block_depth;
        self.new_target_allowed = enclosing_new_target;
    }

    /// Declare each parameter's binding names in the current (function) scope.
    /// Parameters behave like `var` (they coexist with `var`/other params but a
    /// `let`/`const`/`class` in the body clashes with one).
    fn declare_params(&mut self, params: &[Pat]) {
        for p in params {
            let names = bound_names(p);
            for n in &names {
                self.scopes.last_mut().unwrap().vars.insert(n.clone());
            }
        }
    }

    fn check_class(&mut self, c: &ClassDecl) {
        // Class bodies are always strict.
        if let Some(name) = &c.name {
            self.check_strict_binding_name(name, c.span);
        }
        let saved = self.strict;
        self.strict = true;
        // Class heritage is also parsed/evaluated as strict-mode code, while
        // remaining outside the class's own private-name environment.
        if let Some(sc) = &c.superclass {
            self.check_expr(sc);
        }
        let derived = c.superclass.is_some();
        let mut ctor_count = 0;

        // Private names are bound across the entire class body, so collect all
        // declarations before checking computed keys, fields, or methods.
        // A getter/setter pair is the sole permitted duplicate, and both halves
        // must have the same staticness.
        let mut private_names = std::collections::HashSet::new();
        let mut private_decls = std::collections::HashMap::new();
        for m in &c.body {
            let js_syntax::ast::pat::PropKey::Private(name) = &m.key else {
                continue;
            };
            if name == "constructor" {
                self.err(
                    m.span,
                    "a private class element may not be named `#constructor`",
                );
            }
            private_names.insert(name.clone());
            let current = (m.kind, m.static_);
            match private_decls.get_mut(name) {
                None => {
                    private_decls.insert(name.clone(), (current, false));
                }
                Some((previous, paired)) => {
                    let accessor_pair = !*paired
                        && previous.1 == current.1
                        && matches!(
                            (previous.0, current.0),
                            (ClassMemberKind::Get, ClassMemberKind::Set)
                                | (ClassMemberKind::Set, ClassMemberKind::Get)
                        );
                    if accessor_pair {
                        *paired = true;
                    } else {
                        self.err(
                            m.span,
                            format!("private name `#{name}` has already been declared"),
                        );
                    }
                }
            }
        }
        self.private_env.push(private_names);

        for m in &c.body {
            if let js_syntax::ast::pat::PropKey::Computed(key) = &m.key {
                self.check_expr(key);
            }
            let public_name = match &m.key {
                js_syntax::ast::pat::PropKey::Ident(name)
                | js_syntax::ast::pat::PropKey::String(name) => Some(name.as_str()),
                _ => None,
            };
            match &m.value {
                js_syntax::ast::expr::ClassMemberValue::Method(func) => {
                    let is_ctor = matches!(m.kind, ClassMemberKind::Constructor);
                    if is_ctor {
                        ctor_count += 1;
                    }
                    if !m.static_ && public_name == Some("constructor") && !is_ctor {
                        self.err(
                            m.span,
                            "a constructor may not be an async, generator, getter, or setter method",
                        );
                    }
                    if m.static_ && public_name == Some("prototype") {
                        self.err(m.span, "a static class method may not be named `prototype`");
                    }
                    match m.kind {
                        ClassMemberKind::Get if !func.params.is_empty() => {
                            self.err(m.span, "a getter must not have parameters");
                        }
                        ClassMemberKind::Set
                            if func.params.len() != 1
                                || matches!(func.params.first(), Some(Pat::Rest { .. })) =>
                        {
                            self.err(m.span, "a setter must have exactly one non-rest parameter");
                        }
                        _ => {}
                    }
                    // `super.x` is valid in any method/constructor; `super()`
                    // only in a constructor of a derived class.
                    let call_ok = is_ctor && derived;
                    self.check_function(func, true, true, call_ok, true, true);
                }
                js_syntax::ast::expr::ClassMemberValue::Field(init) => {
                    if public_name == Some("constructor") {
                        self.err(m.span, "a class field may not be named `constructor`");
                    }
                    if m.static_ && public_name == Some("prototype") {
                        self.err(m.span, "a static class field may not be named `prototype`");
                    }
                    if let Some(e) = init {
                        if contains_arguments(e) {
                            self.err(
                                e.span(),
                                "a class field initializer may not contain `arguments`",
                            );
                        }
                        // A field initializer has the class as its [[HomeObject]],
                        // so `super.prop` is valid (an arrow nested here inherits
                        // it); only a `super()` *call` is invalid.
                        self.super_prop.push(true);
                        self.super_call.push(false);
                        self.check_expr(e);
                        self.super_prop.pop();
                        self.super_call.pop();
                    }
                }
                // Static initializer blocks: `super.prop` is valid (the static
                // [[HomeObject]] is the class); `super()` is not.
                js_syntax::ast::expr::ClassMemberValue::StaticBlock(body) => {
                    if statements_contain_arguments(body) {
                        self.err(m.span, "a class static block may not contain `arguments`");
                    }
                    // Each static block has its own var/lexical environment;
                    // declarations neither collide with nor leak into adjacent
                    // blocks or the surrounding scope.
                    self.check_statement_list_names(body, m.span);
                    self.enter(true);
                    let enclosing_control = self.take_control_context();
                    self.super_prop.push(true);
                    self.super_call.push(false);
                    let enclosing_new_target =
                        std::mem::replace(&mut self.new_target_allowed, true);
                    self.static_block_depth += 1;
                    for s in body {
                        self.check_stmt(s);
                    }
                    self.static_block_depth -= 1;
                    self.super_prop.pop();
                    self.super_call.pop();
                    self.new_target_allowed = enclosing_new_target;
                    self.restore_control_context(enclosing_control);
                    self.leave();
                }
            }
        }
        if ctor_count > 1 {
            self.err(c.span, "a class may not have more than one constructor");
        }
        self.private_env.pop();
        self.strict = saved;
    }

    fn check_member(&mut self, m: &MemberExpr) {
        // `super.x` / `super[expr]` require a valid super-property context.
        if matches!(m.object.as_ref(), Expr::Super(_)) && !*self.super_prop.last().unwrap() {
            self.err(
                m.span,
                "`super` property is only valid in a class method or constructor",
            );
        }
        if let MemberProp::Private(name) = &m.property {
            if matches!(m.object.as_ref(), Expr::Super(_)) {
                self.err(m.span, "a private name may not be accessed through `super`");
            }
            if !self
                .private_env
                .iter()
                .rev()
                .any(|environment| environment.contains(name))
            {
                self.err(
                    m.span,
                    format!("private name `#{name}` is not declared in an enclosing class"),
                );
            }
        }
        self.check_expr(&m.object);
        if let MemberProp::Computed(e) = &m.property {
            self.check_expr(e);
        }
    }

    fn check_param_list(&mut self, params: &[Pat], non_simple: bool, strict: bool, unique: bool) {
        // Duplicate formal-parameter detection.
        let names: Vec<String> = params.iter().flat_map(bound_names).collect();
        let mut seen = std::collections::HashSet::new();
        for n in &names {
            if !seen.insert(n.clone()) {
                // Duplicate. Always an error for non-simple parameter lists;
                // for simple lists, only in strict mode.
                if non_simple || strict || unique {
                    self.err(
                        Span::DUMMY,
                        if non_simple {
                            "duplicate parameter name in non-simple parameter list"
                        } else {
                            "duplicate parameter name in strict mode"
                        },
                    );
                    break;
                }
            }
        }
        // Per-binding strict checks + structural rest-with-default.
        for p in params {
            self.check_binding_pat_strict(p, strict);
        }
    }

    fn check_parameter_initializers(&mut self, params: &[Pat], strict: bool, super_prop: bool) {
        let saved_strict = self.strict;
        self.strict = strict;
        // YieldExpression and AwaitExpression are forbidden in formal
        // parameter initializers, including generator/async functions.
        self.await_ctx.push(false);
        self.yield_ctx.push(false);
        self.super_prop.push(super_prop);
        self.super_call.push(false);
        for param in params {
            self.check_pattern_expressions(param);
        }
        self.super_call.pop();
        self.super_prop.pop();
        self.yield_ctx.pop();
        self.await_ctx.pop();
        self.strict = saved_strict;
    }

    fn check_pattern_expressions(&mut self, pat: &Pat) {
        match pat {
            Pat::Ident { .. } => {}
            Pat::Array { elements, .. } => {
                for element in elements.iter().flatten() {
                    if let ArrayPatElement::Pat(pat) = element {
                        self.check_pattern_expressions(pat);
                    }
                }
            }
            Pat::Object { properties, .. } => {
                for property in properties {
                    match property {
                        ObjectPatProp::KeyValue { key, value, .. } => {
                            if let js_syntax::ast::pat::PropKey::Computed(expr) = key {
                                self.check_expr(expr);
                            }
                            self.check_pattern_expressions(value);
                        }
                        ObjectPatProp::Rest { arg, .. } => self.check_pattern_expressions(arg),
                    }
                }
            }
            Pat::Rest { arg, .. } => self.check_pattern_expressions(arg),
            Pat::Assignment { left, right, .. } => {
                self.check_pattern_expressions(left);
                self.check_expr(right);
            }
            Pat::Member(member) => {
                if member.optional {
                    self.err(
                        member.span,
                        "an optional chain is not a valid destructuring target",
                    );
                }
                self.check_member(member);
            }
        }
    }

    fn check_binding_pat(&mut self, pat: &Pat) {
        self.check_binding_pat_strict(pat, self.strict);
        self.check_pattern_expressions(pat);
    }

    fn check_assignment_pattern(&mut self, pat: &Pat) {
        match pat {
            Pat::Ident { .. } | Pat::Member(_) => {}
            Pat::Array { elements, .. } => {
                for element in elements.iter().flatten() {
                    if let ArrayPatElement::Pat(pattern) = element {
                        self.check_assignment_pattern(pattern);
                    }
                }
            }
            Pat::Object { properties, .. } => {
                for property in properties {
                    match property {
                        ObjectPatProp::KeyValue { value, .. } => {
                            self.check_assignment_pattern(value)
                        }
                        ObjectPatProp::Rest { arg, .. } => {
                            self.check_assignment_pattern(arg);
                        }
                    }
                }
            }
            Pat::Rest { arg, .. } => {
                self.check_assignment_pattern(arg);
            }
            Pat::Assignment { left, .. } => self.check_assignment_pattern(left),
        }
    }

    /// Walk a binding pattern, enforcing strict-mode name restrictions and the
    /// "rest element may not have a default" structural rule.
    fn check_binding_pat_strict(&mut self, pat: &Pat, strict: bool) {
        match pat {
            Pat::Ident { name, span } => {
                if name == "yield" && *self.yield_ctx.last().unwrap() {
                    self.err(
                        *span,
                        "`yield` is not a valid assignment target in a generator",
                    );
                }
                if name == "await" && *self.await_ctx.last().unwrap() {
                    self.err(
                        *span,
                        "`await` is not a valid assignment target in an async context",
                    );
                }
                if strict {
                    self.check_strict_binding_name_at(name, *span);
                }
            }
            Pat::Array { elements, .. } => {
                for el in elements.iter().flatten() {
                    match el {
                        ArrayPatElement::Pat(p) => {
                            // Rest element with a default is a SyntaxError
                            // (`[...x = 1]`).
                            if let Pat::Rest { arg, .. } = p {
                                if matches!(arg.as_ref(), Pat::Assignment { .. }) {
                                    self.err(
                                        p.span(),
                                        "a rest element may not have a default initializer",
                                    );
                                }
                            }
                            self.check_binding_pat_strict(p, strict);
                        }
                        ArrayPatElement::Hole(_) => {}
                    }
                }
            }
            Pat::Object { properties, .. } => {
                for prop in properties {
                    match prop {
                        ObjectPatProp::KeyValue { key, value, span } => {
                            if matches!(key, js_syntax::ast::pat::PropKey::Private(_)) {
                                self.err(
                                    *span,
                                    "a private name may not be used as an object pattern key",
                                );
                            }
                            self.check_binding_pat_strict(value, strict);
                        }
                        ObjectPatProp::Rest { arg, .. } => {
                            if matches!(arg.as_ref(), Pat::Assignment { .. }) {
                                self.err(
                                    arg.span(),
                                    "a rest element may not have a default initializer",
                                );
                            }
                            self.check_binding_pat_strict(arg, strict);
                        }
                    }
                }
            }
            Pat::Rest { arg, .. } => {
                if matches!(arg.as_ref(), Pat::Assignment { .. }) {
                    self.err(
                        pat.span(),
                        "a rest parameter may not have a default initializer",
                    );
                }
                self.check_binding_pat_strict(arg, strict);
            }
            Pat::Assignment { left, .. } => self.check_binding_pat_strict(left, strict),
            // Member targets only occur in assignment destructuring, never in a
            // binding pattern — no strict binding-name rules apply.
            Pat::Member(_) => {}
        }
    }

    /// `eval`/`arguments` and strict FutureReservedWords are not permitted as
    /// binding names in strict mode.
    fn check_strict_binding_name(&mut self, name: &str, span: Span) {
        self.check_strict_binding_name_at(name, span);
    }

    fn check_strict_binding_name_at(&mut self, name: &str, span: Span) {
        if self.static_block_depth > 0 && name == "await" {
            self.err(
                span,
                "`await` may not be used as a binding inside a class static block",
            );
            return;
        }
        if name == "eval" || name == "arguments" {
            self.err(
                span,
                format!("`{}` is not a valid binding name in strict mode", name),
            );
            return;
        }
        if is_strict_future_reserved_word(name) {
            self.err(
                span,
                format!(
                    "`{}` is a reserved word in strict mode and cannot be a binding",
                    name
                ),
            );
        }
    }

    fn check_expr(&mut self, expr: &Expr) {
        match expr {
            Expr::Function(f) => self.check_function(f, false, false, false, false, false),
            Expr::Arrow(a) => self.check_arrow(a),
            Expr::Class(c) => self.check_class(c),
            Expr::Assign {
                op, left, right, ..
            } => {
                // Strict mode: `eval`/`arguments` are not assignment targets.
                if self.strict {
                    if let AssignTarget::Ident { name, span } = left {
                        if name == "eval" || name == "arguments" {
                            self.err(
                                *span,
                                format!("`{}` cannot be assigned in strict mode", name),
                            );
                        }
                    }
                }
                // Logical compound assignments (&&=, ||=, ??=) on those targets
                // are also disallowed; covered by the same check above.
                let _ = op;
                self.check_assign_target(left);
                self.check_expr(right);
            }
            Expr::Unary { op, arg, .. } => {
                if *op == UnaryOp::Delete {
                    if self.strict {
                        if let Some(span) = parenthesized_identifier_reference(arg) {
                            self.err(span, "`delete` of an unqualified identifier is not allowed in strict mode");
                        }
                    }
                    if private_member_reference(arg) {
                        self.err(arg.span(), "a private class element may not be deleted");
                    }
                }
                self.check_expr(arg);
            }
            Expr::Update { arg, span, .. } => {
                if !is_simple_assignment_target(arg) {
                    self.err(
                        *span,
                        "invalid update target: operand is not a simple assignment target",
                    );
                }
                if self.strict {
                    if let Expr::Ident { name, span } = arg.as_ref() {
                        if name == "eval" || name == "arguments" {
                            self.err(
                                *span,
                                format!("`{}` cannot be updated in strict mode", name),
                            );
                        }
                    }
                }
                self.check_expr(arg);
            }
            Expr::Binary { left, right, .. } | Expr::Logical { left, right, .. } => {
                self.check_expr(left);
                self.check_expr(right);
            }
            Expr::PrivateIn { span, name, right } => {
                if !self
                    .private_env
                    .iter()
                    .rev()
                    .any(|environment| environment.contains(name))
                {
                    self.err(
                        *span,
                        format!("private name `#{name}` is not declared in an enclosing class"),
                    );
                }
                self.check_expr(right);
            }
            Expr::Conditional {
                test, cons, alt, ..
            } => {
                self.check_expr(test);
                self.check_expr(cons);
                self.check_expr(alt);
            }
            Expr::Sequence { exprs, .. } => {
                for e in exprs {
                    self.check_expr(e);
                }
            }
            Expr::Member(m) => self.check_member(m),
            Expr::Call(c) => {
                // `super(...)` requires a derived-class constructor context.
                if matches!(c.callee.as_ref(), Expr::Super(_)) && !*self.super_call.last().unwrap()
                {
                    self.err(
                        c.span,
                        "`super()` call is only valid in a derived class constructor",
                    );
                }
                self.check_expr(&c.callee);
                for a in &c.args {
                    if let js_syntax::ast::expr::CallArg::Expr(e) = a {
                        self.check_expr(e);
                    }
                }
            }
            Expr::New(n) => {
                // `new import(...)` (and the phase forms) is a SyntaxError.
                if let Expr::ImportCall { span, .. } = n.callee.as_ref() {
                    self.err(*span, "`import(...)` cannot be the callee of `new`");
                }
                self.check_expr(&n.callee);
                for a in &n.args {
                    if let js_syntax::ast::expr::CallArg::Expr(e) = a {
                        self.check_expr(e);
                    }
                }
            }
            Expr::Array { elements, .. } => {
                for el in elements.iter().flatten() {
                    match el {
                        js_syntax::ast::expr::ArrayExprElement::Expr(e)
                        | js_syntax::ast::expr::ArrayExprElement::Spread(e) => self.check_expr(e),
                    }
                }
            }
            Expr::Object { props, .. } => {
                let mut proto_count = 0;
                for p in props {
                    if p.shorthand
                        && matches!(
                            p.value,
                            js_syntax::ast::expr::ObjectPropValue::Expr(Expr::Assign { .. })
                        )
                    {
                        self.err(
                            p.span,
                            "a cover initialized name is only valid in an assignment pattern",
                        );
                    }
                    // Only a `__proto__: value` *data* property (the prototype
                    // setter form) counts toward the duplicate-`__proto__`
                    // SyntaxError. Methods, getters, setters, shorthand, and
                    // computed keys do not.
                    let is_proto_data = !p.computed
                        && !p.method
                        && p.shorthand == false
                        && matches!(p.kind, js_syntax::ast::expr::ObjectPropKind::Init)
                        && matches!(
                            &p.key,
                            js_syntax::ast::pat::PropKey::String(s)
                                | js_syntax::ast::pat::PropKey::Ident(s)
                            if s == "__proto__"
                        )
                        && matches!(p.value, js_syntax::ast::expr::ObjectPropValue::Expr(_));
                    if is_proto_data {
                        proto_count += 1;
                    }
                    match &p.value {
                        js_syntax::ast::expr::ObjectPropValue::Expr(e) => self.check_expr(e),
                        js_syntax::ast::expr::ObjectPropValue::Method(f) => {
                            // `super.x` is valid inside an object-literal method
                            // (resolves via the object's prototype); `super()`
                            // never is.
                            self.check_function(f, false, true, false, true, true)
                        }
                        js_syntax::ast::expr::ObjectPropValue::Spread(e) => self.check_expr(e),
                    }
                }
                if proto_count > 1 {
                    self.err(
                        expr.span(),
                        "duplicate `__proto__` property in object literal",
                    );
                }
            }
            Expr::TemplateLit { expressions, .. } => {
                for e in expressions {
                    self.check_expr(e);
                }
            }
            Expr::Paren { expr, .. } => self.check_expr(expr),
            Expr::Spread { arg, .. } => self.check_expr(arg),
            Expr::TaggedTemplate { tag, .. } => self.check_expr(tag),
            Expr::ImportCall {
                source, options, ..
            } => {
                self.check_expr(source);
                if let Some(o) = options {
                    self.check_expr(o);
                }
            }
            Expr::ImportMeta(span) => {
                if !self.is_module {
                    self.err(*span, "`import.meta` is only valid when parsing a module");
                }
            }
            Expr::NewTarget(span) => {
                if !self.new_target_allowed {
                    self.err(*span, "`new.target` is only valid within function code");
                }
            }
            Expr::Yield { arg, span, .. } => {
                if self.static_block_depth > 0 {
                    self.err(*span, "a class static block may not contain `yield`");
                } else if !*self.yield_ctx.last().unwrap() {
                    self.err(*span, "`yield` is only valid within a generator function");
                }
                if let Some(a) = arg {
                    self.check_expr(a);
                }
            }
            Expr::Await { arg, span } => {
                if self.static_block_depth > 0 {
                    self.err(*span, "a class static block may not contain `await`");
                } else if !*self.await_ctx.last().unwrap() {
                    self.err(*span, "`await` is only valid within async functions");
                }
                self.check_expr(arg);
            }
            Expr::Super(span) => {
                // Bare `super` must be a member access or call; both are handled
                // at the Member/Call level. A bare reference is invalid.
                if !*self.super_prop.last().unwrap() {
                    self.err(
                        *span,
                        "`super` is only valid in a class method or constructor",
                    );
                }
            }
            Expr::Ident { name, span } => {
                if self.strict && is_strict_future_reserved_word(name) {
                    self.err(*span, format!("`{name}` is reserved in strict mode"));
                }
                if self.static_block_depth > 0 && name == "await" {
                    self.err(*span, "a class static block may not contain `await`");
                }
            }
            Expr::This(_) | Expr::Regex { .. } => {}
            Expr::Lit(lit) => {
                // Legacy octal (`077`, `010`) is a SyntaxError in strict mode.
                if self.strict {
                    match lit {
                        Lit::Number(span, _, raw) if is_legacy_octal(raw) => {
                            self.err(
                                *span,
                                "legacy octal literals are not allowed in strict mode",
                            );
                        }
                        Lit::String(span, _, true) => {
                            self.err(
                                *span,
                                "legacy escape sequences are not allowed in strict mode",
                            );
                        }
                        _ => {}
                    }
                }
            }
        }
    }

    fn check_assign_target(&mut self, target: &AssignTarget) {
        match target {
            AssignTarget::Pat(pat) => self.check_binding_pat(pat),
            AssignTarget::Member(m) => {
                if !member_is_simple_assignment_target(m) {
                    self.err(
                        m.span,
                        "invalid assignment target: an optional chaining is not assignable",
                    );
                }
                self.check_member(m);
            }
            AssignTarget::Ident { name, span } => {
                if self.strict && is_strict_future_reserved_word(name) {
                    self.err(
                        *span,
                        format!("`{name}` is not a valid assignment target in strict mode"),
                    );
                }
            }
        }
    }

    fn check_arrow(&mut self, a: &js_syntax::ast::expr::ArrowExpr) {
        // Arrow functions are strict iff their enclosing context is strict, or
        // (for a block body) they have their own directive prologue.
        let has_use_strict = match &a.body {
            ArrowBody::Block(stmts) => contains_use_strict(stmts),
            ArrowBody::Expr(_) => false,
        };
        let body_strict = match &a.body {
            ArrowBody::Block(_) => self.strict || has_use_strict,
            ArrowBody::Expr(_) => self.strict,
        };
        let non_simple = !is_simple_parameter_list(&a.params);
        if has_use_strict && non_simple {
            self.err(
                a.span,
                "an arrow function with a non-simple parameter list may not contain a `use strict` directive",
            );
        }
        self.enter(true);
        let enclosing_static_block_depth = self.static_block_depth;
        let enclosing_control = self.take_control_context();
        self.declare_params(&a.params);
        self.check_param_list(&a.params, non_simple, body_strict, true);
        // Arrows inherit await-ness (own async OR enclosing); they are never
        // generators. They also inherit super context (transparent).
        let inherited_await = *self.await_ctx.last().unwrap();
        self.await_ctx.push(a.is_async || inherited_await);
        self.yield_ctx.push(false);
        let sp = *self.super_prop.last().unwrap();
        let sc = *self.super_call.last().unwrap();
        self.check_parameter_initializers(&a.params, body_strict, sp);
        // Arrow parameters are parsed in the surrounding static-block context;
        // only the arrow body crosses the function boundary for ContainsAwait.
        self.static_block_depth = 0;
        self.super_prop.push(sp);
        self.super_call.push(sc);
        let saved = self.strict;
        self.strict = body_strict;
        match &a.body {
            ArrowBody::Block(stmts) => {
                for s in stmts {
                    self.check_stmt(s);
                }
            }
            ArrowBody::Expr(e) => self.check_expr(e),
        }
        self.strict = saved;
        self.await_ctx.pop();
        self.yield_ctx.pop();
        self.super_prop.pop();
        self.super_call.pop();
        self.leave();
        self.restore_control_context(enclosing_control);
        self.static_block_depth = enclosing_static_block_depth;
    }
}

fn label_targets_iteration(stmt: &Stmt) -> bool {
    match stmt {
        Stmt::While { .. }
        | Stmt::DoWhile { .. }
        | Stmt::For { .. }
        | Stmt::ForIn { .. }
        | Stmt::ForOf { .. } => true,
        Stmt::Labeled { body, .. } => label_targets_iteration(body),
        _ => false,
    }
}

fn decl_is_using(declaration: &Decl) -> bool {
    matches!(
        declaration,
        Decl::Var {
            kind: js_syntax::ast::stmt::VarKind::Using | js_syntax::ast::stmt::VarKind::AwaitUsing,
            ..
        }
    )
}

fn stmt_is_using_declaration(statement: &Stmt) -> bool {
    matches!(statement, Stmt::Decl(declaration) if decl_is_using(declaration))
}

/// Declaration forms are not Statements and therefore cannot directly follow
/// a label. Annex B permits one narrow exception in sloppy scripts: an
/// ordinary, synchronous FunctionDeclaration.
fn labelled_body_declaration_is_invalid(statement: &Stmt, strict: bool) -> bool {
    let Stmt::Decl(declaration) = statement else {
        return false;
    };
    match declaration.as_ref() {
        Decl::Var { kind, .. } => !matches!(kind, js_syntax::ast::stmt::VarKind::Var),
        Decl::Class(_) => true,
        Decl::Function(function) => strict || function.is_async || function.is_generator,
        Decl::Import { .. } | Decl::Export { .. } => true,
    }
}

fn program_item_is_using_declaration(item: &ProgramItem) -> bool {
    match item {
        ProgramItem::Stmt(statement) => stmt_is_using_declaration(statement),
        ProgramItem::Decl(declaration) => decl_is_using(declaration),
    }
}

fn program_item_span(item: &ProgramItem) -> Span {
    match item {
        ProgramItem::Stmt(statement) => statement.span(),
        ProgramItem::Decl(declaration) => declaration.span(),
    }
}

/// Whether an iteration statement's body is a FunctionDeclaration, possibly
/// behind one or more labels. This is an early error even in sloppy code.
fn is_labelled_function(stmt: &Stmt) -> bool {
    match stmt {
        Stmt::Decl(decl) => matches!(decl.as_ref(), Decl::Function(_)),
        Stmt::Labeled { body, .. } => is_labelled_function(body),
        _ => false,
    }
}

/// Parentheses do not hide the private reference targeted by `delete`.
fn private_member_reference(expr: &Expr) -> bool {
    match expr {
        Expr::Member(m) => matches!(m.property, MemberProp::Private(_)),
        Expr::Paren { expr, .. } => private_member_reference(expr),
        _ => false,
    }
}

/// Parenthesization does not change the IdentifierReference targeted by the
/// strict-mode `delete` early error.
fn parenthesized_identifier_reference(expr: &Expr) -> Option<Span> {
    match expr {
        Expr::Ident { span, .. } => Some(*span),
        Expr::Paren { expr, .. } => parenthesized_identifier_reference(expr),
        _ => None,
    }
}

/// Whether `e` is the result of an optional chaining (`a?.b`, `a?.[x]`,
/// `a?.()`), which is never a valid assignment / update target.
fn is_simple_assignment_target(expression: &Expr) -> bool {
    match expression {
        Expr::Paren { expr, .. } => is_simple_assignment_target(expr),
        Expr::Ident { .. } => true,
        Expr::Member(member) => member_is_simple_assignment_target(member),
        _ => false,
    }
}

fn member_is_simple_assignment_target(member: &MemberExpr) -> bool {
    !member.optional && !crate::expr::has_unparenthesized_optional_chain(&member.object)
}

/// Strict-mode FutureReservedWords (Annex B / 12.6.2): these may not be used as
/// binding identifiers in strict code.
fn is_strict_future_reserved_word(s: &str) -> bool {
    matches!(
        s,
        "implements"
            | "interface"
            | "let"
            | "package"
            | "private"
            | "protected"
            | "public"
            | "static"
            | "yield"
    )
}

/// Whether a raw numeric literal is a *legacy octal* form (`0` followed by more
/// digits, not a radix prefix). Such forms (`077`, `010`) are SyntaxErrors in
/// strict mode. `0`, `0.5`, `0x1`, `0b1`, `0o1`, `0e0` are not legacy octal.
fn is_legacy_octal(raw: &str) -> bool {
    let s = raw.strip_suffix('n').unwrap_or(raw);
    if s.len() < 2 {
        return false;
    }
    if !s.starts_with('0') {
        return false;
    }
    let second = s.as_bytes()[1] as char;
    // Radix prefixes and decimal fractions/exponents are not legacy octal.
    if matches!(second, 'x' | 'X' | 'b' | 'B' | 'o' | 'O' | '.' | 'e' | 'E') {
        return false;
    }
    // `_` separators don't change the form.
    second.is_ascii_digit() || second == '_'
}

#[cfg(test)]
mod tests {
    /// Run the full parse pipeline (syntactic + early errors) and collect the
    /// resulting diagnostic messages (empty if accepted).
    fn check_src(src: &str) -> Vec<String> {
        match crate::parse(src) {
            Ok(_) => Vec::new(),
            Err(ds) => ds.into_iter().map(|d| d.message).collect(),
        }
    }

    #[test]
    fn duplicate_params_strict() {
        // `function f(a, a) { "use strict"; }` → strict body, simple params, dup.
        let errs = check_src("function f(a, a) { \"use strict\"; }");
        assert!(
            errs.iter().any(|m| m.contains("duplicate parameter")),
            "{:?}",
            errs
        );
    }

    #[test]
    fn duplicate_params_nonsimple() {
        // Non-simple (default) → duplicate always an error.
        let errs = check_src("function f(a, a = 1) {}");
        assert!(
            errs.iter().any(|m| m.contains("duplicate parameter")),
            "{:?}",
            errs
        );
    }

    #[test]
    fn eval_binding_strict() {
        let errs = check_src("\"use strict\"; var eval = 1");
        assert!(errs.iter().any(|m| m.contains("eval")), "{:?}", errs);
    }

    #[test]
    fn rest_with_default() {
        let errs = check_src("var [...x = 1] = []");
        assert!(errs.iter().any(|m| m.contains("rest")), "{:?}", errs);
    }

    #[test]
    fn parameter_rest_with_default() {
        let errs = check_src("function f(...x = []) {}");
        assert!(
            errs.iter().any(|m| m.contains("rest parameter")),
            "{errs:?}"
        );
    }

    #[test]
    fn non_simple_parameters_cannot_have_use_strict_directive() {
        for src in [
            "function f(a = 0) { 'use strict'; }",
            "({ m([a]) { 'use strict'; } })",
            "([a]) => { 'use strict'; }",
            "class C { m(...a) { 'use strict'; } }",
        ] {
            let errs = check_src(src);
            assert!(
                errs.iter().any(|m| m.contains("non-simple")),
                "{src}: {errs:?}"
            );
        }
    }

    #[test]
    fn parenthesized_use_strict_is_not_a_directive() {
        let errs = check_src("function f(a = 0) { ('use strict'); }");
        assert!(errs.is_empty(), "{errs:?}");
    }

    #[test]
    fn arguments_in_class_field_initializer() {
        for src in [
            "class C { x = arguments; }",
            "class C { x = () => arguments; }",
            "class C { x = () => { let f = () => arguments; }; }",
        ] {
            let errs = check_src(src);
            assert!(
                errs.iter().any(|m| m.contains("field initializer")),
                "{src}: {errs:?}"
            );
        }
    }

    #[test]
    fn ordinary_function_stops_class_field_contains_arguments() {
        let errs = check_src("class C { x = function() { return arguments; }; }");
        assert!(errs.is_empty(), "{errs:?}");
    }

    #[test]
    fn private_names_are_lexically_scoped_and_allow_forward_references() {
        for src in [
            "class C { m() { return this.#x; } #x; }",
            "class Outer { #x; m() { return class Inner { n(o) { return o.#x; } }; } }",
        ] {
            let errs = check_src(src);
            assert!(errs.is_empty(), "{src}: {errs:?}");
        }

        for src in [
            "this.#x;",
            "class C { m() { return this.#missing; } }",
            "class C extends this.#x { #x; }",
        ] {
            let errs = check_src(src);
            assert!(
                errs.iter().any(|m| m.contains("not declared")),
                "{src}: {errs:?}"
            );
        }
    }

    #[test]
    fn private_name_declaration_rules() {
        for src in [
            "class C { #x; #x; }",
            "class C { get #x() {} get #x() {} }",
            "class C { static get #x() {} set #x(v) {} }",
            "class C { #constructor; }",
        ] {
            assert!(!check_src(src).is_empty(), "{src}");
        }

        let errs = check_src("class C { get #x() {} set #x(v) {} }");
        assert!(errs.is_empty(), "{errs:?}");
    }

    #[test]
    fn private_references_cannot_use_super_or_delete() {
        for src in [
            "class C { #x; m() { delete this.#x; } }",
            "class C { #x; m() { delete ((this.#x)); } }",
            "class C { #x; m() { return super.#x; } }",
        ] {
            assert!(!check_src(src).is_empty(), "{src}");
        }
    }

    #[test]
    fn class_element_name_and_arity_rules() {
        for src in [
            "class C { constructor; }",
            "class C { static 'constructor'; }",
            "class C { static prototype; }",
            "class C { static get prototype() {} }",
            "class C { async constructor() {} }",
            "class C { *constructor() {} }",
            "class C { get constructor() {} }",
            "class C { get x(value) {} }",
            "class C { set x() {} }",
            "class C { set x(...value) {} }",
        ] {
            assert!(!check_src(src).is_empty(), "{src}");
        }

        for src in [
            "class C { constructor() {} }",
            "class C { static constructor() {} }",
            "class C { get x() {} set x(value) {} }",
        ] {
            let errs = check_src(src);
            assert!(errs.is_empty(), "{src}: {errs:?}");
        }
    }

    #[test]
    fn class_method_parameter_initializers_use_parameter_context() {
        for src in [
            "class C { m(x = yield) {} }",
            "class C { *m(x = yield) {} }",
            "class C { async m(x = await 0) {} }",
            "class C { m(x = super()) {} }",
        ] {
            assert!(!check_src(src).is_empty(), "{src}");
        }
    }

    #[test]
    fn class_heritage_and_nested_functions_remain_strict() {
        for src in [
            "class C extends (function() { with ({}) {} }()) {}",
            "class C { *g() { function h() { yield = 1; } } }",
        ] {
            assert!(!check_src(src).is_empty(), "{src}");
        }
    }

    #[test]
    fn class_static_block_await_and_private_pattern_rules() {
        for src in [
            "class C { static { class await {} } }",
            "class C { static { let await; } }",
            "class C { static { function await() {} } }",
            "class C { static { arguments; } }",
            "async function f() { class C { static { await 0; } } }",
            "function* g() { class C { static { yield; } } }",
            "function f() { class C { static { return; } } }",
            "class C { #x; m() { const { #x: x } = this; } }",
        ] {
            assert!(!check_src(src).is_empty(), "{src}");
        }
        for src in [
            "class C { static { function f() { let await; arguments; } } }",
            "class C { static { (function await(await) {}); } }",
            "class C { static { (function * await(await) {}); } }",
            "let x; class C { static { let x; } static { let x; } }",
        ] {
            let errs = check_src(src);
            assert!(errs.is_empty(), "{src}: {errs:?}");
        }
    }

    #[test]
    fn labels_and_jump_targets_respect_function_boundaries() {
        for src in [
            "x: x: 0;",
            "x: while (false) { break y; }",
            "x: while (false) { continue y; }",
            "x: { continue x; }",
            "break;",
            "continue;",
            "x: function f() { break x; }",
        ] {
            assert!(!check_src(src).is_empty(), "{src}");
        }
        for src in [
            "x: { break x; }",
            "x: y: while (false) { continue x; continue y; }",
            "while (false) { break; continue; }",
            "switch (0) { default: break; }",
        ] {
            let errs = check_src(src);
            assert!(errs.is_empty(), "{src}: {errs:?}");
        }
    }

    #[test]
    fn switch_case_block_declaration_sets_are_checked_together() {
        for src in [
            "switch (0) { case 0: let x; default: const x = 1; }",
            "switch (0) { case 0: class x {} default: var x; }",
            "switch (0) { case 0: function x() {} default: var x; }",
            "switch (0) { case 0: let x; default: { var x; } }",
            "'use strict'; switch (0) { case 0: function x() {} default: function x() {} }",
        ] {
            assert!(!check_src(src).is_empty(), "{src}");
        }

        for src in [
            "switch (0) { case 0: var x; default: var x; }",
            "switch (0) { case 0: function x() {} default: function x() {} }",
            "switch (0) { case 0: let x; default: { let x; } }",
        ] {
            let errs = check_src(src);
            assert!(errs.is_empty(), "{src}: {errs:?}");
        }
    }

    #[test]
    fn block_declaration_sets_use_static_semantic_boundaries() {
        for src in [
            "{ function f() {} var f; }",
            "{ var f; async function f() {} }",
            "{ class f {} { var f; } }",
            "{ { var f; } function* f() {} }",
            "'use strict'; { function f() {} function f() {} }",
            "try { function f() {} { var f; } } finally {}",
            "try {} catch (f) { function f() {} }",
        ] {
            assert!(!check_src(src).is_empty(), "{src}");
        }

        for src in [
            "let f; { function f() {} }",
            "{ { let f; } var f; }",
            "{ function f() {} function f() {} }",
            "{ function f() {} } { var f; }",
        ] {
            let errs = check_src(src);
            assert!(errs.is_empty(), "{src}: {errs:?}");
        }
    }

    #[test]
    fn script_module_goal_and_new_target_boundaries() {
        for src in [
            "import x from 'x';",
            "export var x;",
            "new.target;",
            "(() => new.target)();",
        ] {
            assert!(!check_src(src).is_empty(), "{src}");
        }

        for src in [
            "function f() { new.target; }",
            "function f() { return () => new.target; }",
            "function f(x = new.target) {}",
            "class C { static { new.target; } }",
        ] {
            let errs = check_src(src);
            assert!(errs.is_empty(), "{src}: {errs:?}");
        }
    }

    #[test]
    fn for_in_of_assignment_patterns_receive_early_error_checks() {
        for src in [
            "'use strict'; for ({ eval } of values) {}",
            "for ([obj?.x] of values) {}",
            "for ([...[(x, y)]] in source) {}",
            "for ([...x = 1] of values) {}",
            "function* g() { for ({ yield } of values) {} }",
            "for (const x of values) { var x; }",
            "for (let let of values) {}",
            "for (x of values) label: function f() {}",
        ] {
            assert!(!check_src(src).is_empty(), "{src}");
        }
        for src in [
            "for ([obj.x = 1, ...[rest]] of values) {}",
            "for (var let of values) {}",
            "for (x of values) { var x; }",
        ] {
            let errs = check_src(src);
            assert!(errs.is_empty(), "{src}: {errs:?}");
        }
    }

    #[test]
    fn allows_sloppy_duplicates() {
        // Sloppy mode + simple params: duplicates allowed.
        let errs = check_src("function f(a, a) {}");
        assert!(errs.is_empty(), "{:?}", errs);
    }

    #[test]
    fn lexical_redeclaration() {
        let errs = check_src("let a; let a;");
        assert!(
            errs.iter().any(|m| m.contains("already been declared")),
            "{:?}",
            errs
        );
    }

    #[test]
    fn let_vs_var_clash() {
        let errs = check_src("let a; var a;");
        assert!(
            errs.iter().any(|m| m.contains("already been declared")),
            "{:?}",
            errs
        );
    }

    #[test]
    fn lexical_in_separate_scopes_ok() {
        // `let a` in a nested block does not clash with the outer `let a`.
        let errs = check_src("let a; { let a; }");
        assert!(errs.is_empty(), "{:?}", errs);
    }

    #[test]
    fn param_vs_body_let_clash() {
        let errs = check_src("function f(x){ let x; }");
        assert!(
            errs.iter().any(|m| m.contains("already been declared")),
            "{:?}",
            errs
        );
    }

    #[test]
    fn param_vs_body_var_ok() {
        // `var x` redeclaring a parameter is allowed.
        let errs = check_src("function f(x){ var x; }");
        assert!(errs.is_empty(), "{:?}", errs);
    }

    #[test]
    fn duplicate_class_constructor() {
        let errs = check_src("class C { constructor(){} constructor(){} }");
        assert!(errs.iter().any(|m| m.contains("constructor")), "{:?}", errs);
    }

    #[test]
    fn static_constructor_is_not_constructor() {
        // `static constructor` is a static method, not a duplicate constructor.
        let errs = check_src("class C { static constructor(){} constructor(){} }");
        assert!(errs.is_empty(), "{:?}", errs);
    }

    #[test]
    fn for_of_const_no_initializer_ok() {
        // `for (const x of arr)` needs no initializer.
        let errs = check_src("for (const x of [1,2]) { x; }");
        assert!(errs.is_empty(), "{:?}", errs);
    }

    // ---- await / yield / super context ----

    #[test]
    fn yield_outside_generator_is_identifier() {
        // In a sloppy ordinary function, `yield` is a plain identifier
        // reference — `yield;` is a valid (no-op) expression statement.
        let errs = check_src("function f(){ yield; }");
        assert!(errs.is_empty(), "{:?}", errs);
    }

    #[test]
    fn await_outside_async_is_identifier() {
        // In a sloppy ordinary function, `await` is a plain identifier
        // reference — `await;` is a valid (no-op) expression statement.
        let errs = check_src("function f(){ await; }");
        assert!(errs.is_empty(), "{:?}", errs);
    }

    #[test]
    fn yield_in_generator_ok() {
        let errs = check_src("function* g(){ yield 1; }");
        assert!(errs.is_empty(), "{:?}", errs);
    }

    #[test]
    fn await_in_async_ok() {
        let errs = check_src("async function f(){ await 1; }");
        assert!(errs.is_empty(), "{:?}", errs);
    }

    #[test]
    fn await_inherited_by_nested_arrow() {
        // A non-async arrow inside an async function inherits await.
        let errs = check_src("async function f(){ var a = () => await 1; }");
        assert!(errs.is_empty(), "{:?}", errs);
    }

    #[test]
    fn super_call_only_in_derived_ctor() {
        // super() in a non-derived method/ctor is invalid.
        let errs = check_src("class C { m(){ super() } }");
        assert!(errs.iter().any(|m| m.contains("super")), "{:?}", errs);
    }

    #[test]
    fn super_call_in_derived_ctor_ok() {
        let errs = check_src("class C extends D { constructor(){ super() } }");
        assert!(errs.is_empty(), "{:?}", errs);
    }

    #[test]
    fn super_property_in_method_ok() {
        let errs = check_src("class C { m(){ return super.x } }");
        assert!(errs.is_empty(), "{:?}", errs);
    }

    #[test]
    fn super_property_in_object_method_ok() {
        let errs = check_src("var o = { m(){ return super.x } }");
        assert!(errs.is_empty(), "{:?}", errs);
    }

    #[test]
    fn super_in_field_initializer_invalid() {
        // `super.prop` is valid in a field initializer (the field's
        // [[HomeObject]] is the class); only a `super()` *call* is invalid.
        let errs = check_src("class C { x = super(); }");
        assert!(errs.iter().any(|m| m.contains("super")), "{:?}", errs);
    }

    #[test]
    fn super_prop_in_field_initializer_ok() {
        let errs = check_src("class C extends B { x = super.y; }");
        assert!(errs.is_empty(), "{:?}", errs);
    }
}
