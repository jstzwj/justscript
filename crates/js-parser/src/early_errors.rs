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
use js_syntax::ast::expr::{ArrowBody, AssignTarget, Expr};
use js_syntax::ast::lit::Lit;
use js_syntax::ast::op::UnaryOp;
use js_syntax::ast::pat::{ArrayPatElement, ObjectPatProp, Pat};
use js_syntax::ast::stmt::{Decl, Stmt};
use js_syntax::ast::{ClassDecl, FunctionDecl, Program, ProgramItem, ProgramKind};
use js_syntax::Span;

/// Run all early-error checks against a parsed [`Program`].
pub fn check(program: &Program) -> Vec<Diagnostic> {
    let is_module = program.kind == ProgramKind::Module;
    let mut c = Checker {
        errors: Vec::new(),
        strict: is_module || strict_directive_items(&program.body),
        scopes: vec![Scope::new(true)],
        // Modules allow top-level `await`; classic scripts do not. `yield` is
        // never valid at the top level.
        await_ctx: vec![is_module],
        yield_ctx: vec![false],
        super_prop: vec![false],
        super_call: vec![false],
    };
    for item in &program.body {
        c.check_item(item);
    }
    c.errors
}

/// One lexical scope: block, function-body, catch, or for-loop. `lexical`
/// holds `let`/`const`/`class` (and function-scope params); `vars` holds
/// hoisted `var`/`function` declarations.
#[derive(Default)]
struct Scope {
    lexical: std::collections::HashMap<String, Span>,
    vars: std::collections::HashSet<String>,
    is_function: bool,
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
}

impl Checker {
    fn err(&mut self, span: Span, msg: impl Into<String>) {
        self.errors.push(Diagnostic::error(span, msg));
    }

    // ---- scope helpers --------------------------------------------------

    fn enter(&mut self, is_function: bool) {
        self.scopes.push(Scope::new(is_function));
    }

    fn leave(&mut self) {
        self.scopes.pop();
    }

    /// Declare a lexical binding (`let`/`const`/`class`, or a param) in the
    /// current scope. Errors on a same-scope collision with another lexical or
    /// a `var`/`function` binding.
    fn declare_lexical(&mut self, name: &str, span: Span) {
        let scope = self.scopes.last_mut().unwrap();
        if let Some(_prev) = scope.lexical.get(name) {
            self.err(span, format!("identifier `{}` has already been declared", name));
            return;
        }
        if scope.vars.contains(name) {
            self.err(span, format!("identifier `{}` has already been declared", name));
            return;
        }
        scope.lexical.insert(name.to_string(), span);
    }

    /// Declare a hoisted `var`/`function` binding in the nearest function
    /// scope. Errors if a lexical binding of the same name exists in that
    /// function scope.
    fn declare_var(&mut self, name: &str, span: Span) {
        // `var`/`function` hoist to the nearest function scope.
        let fn_idx = self
            .scopes
            .iter()
            .rposition(|s| s.is_function)
            .unwrap_or(0);
        let scope = &mut self.scopes[fn_idx];
        if scope.lexical.contains_key(name) {
            self.err(span, format!("identifier `{}` has already been declared", name));
            return;
        }
        scope.vars.insert(name.to_string());
    }

    fn check_item(&mut self, item: &ProgramItem) {
        match item {
            ProgramItem::Stmt(s) => self.check_stmt(s),
            ProgramItem::Decl(d) => self.check_decl(d),
        }
    }

    /// Validate the body of an unbraced `if`/`for`/`while`/`do` clause, then
    /// recurse. A non-block single-statement body may not be a lexical
    /// (`let`/`const`), class, or function declaration — those require a block.
    fn check_unbraced_body(&mut self, body: &Stmt) {
        // A non-block single-statement body may not be a lexical
        // (`let`/`const`/`using`), class, or function declaration — those
        // require a block. (`var` is permitted.)
        let bad = if let Stmt::Decl(d) = body {
            matches!(
                d.as_ref(),
                Decl::Class(_) | Decl::Function(_)
            ) || matches!(d.as_ref(), Decl::Var { kind, .. } if matches!(
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
                self.enter(false);
                for s in body {
                    self.check_stmt(s);
                }
                self.leave();
            }
            Stmt::Expr { expr, .. } => self.check_expr(expr),
            Stmt::Decl(d) => self.check_decl(d),
            Stmt::If { test, cons, alt, .. } => {
                self.check_expr(test);
                self.check_unbraced_body(cons);
                if let Some(a) = alt {
                    self.check_unbraced_body(a);
                }
            }
            Stmt::While { test, body, .. } | Stmt::DoWhile { test, body, .. } => {
                self.check_expr(test);
                self.check_unbraced_body(body);
            }
            Stmt::For {
                init, test, update, body, ..
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
                self.check_unbraced_body(body);
                self.leave();
            }
            Stmt::ForIn { left, right, body, .. } | Stmt::ForOf { left, right, body, .. } => {
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
                    self.check_decl_opts(d, true);
                }
                self.check_expr(right);
                self.check_unbraced_body(body);
                self.leave();
            }
            Stmt::Switch { disc, cases, .. } => {
                self.check_expr(disc);
                // A switch is one block scope shared by all case bodies.
                self.enter(false);
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
                self.leave();
                if default_count > 1 {
                    self.err(stmt.span(), "switch may have at most one `default` clause");
                }
            }
            Stmt::Throw { arg, .. } => self.check_expr(arg),
            Stmt::Try { block, handler, finalizer, .. } => {
                self.enter(false);
                for s in &block.body {
                    self.check_stmt(s);
                }
                self.leave();
                if let Some(h) = handler {
                    // The catch parameter shares a scope with the catch body.
                    self.enter(false);
                    if let Some(p) = &h.param {
                        let mut names = Vec::new();
                        collect_binding_names(p, &mut names);
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
                    self.enter(false);
                    for s in f {
                        self.check_stmt(s);
                    }
                    self.leave();
                }
            }
            Stmt::Labeled { body, .. } => self.check_stmt(body),
            Stmt::With { obj, body, .. } => {
                self.check_expr(obj);
                if self.strict {
                    self.err(stmt.span(), "`with` is not allowed in strict mode");
                }
                self.check_stmt(body);
            }
            Stmt::Return { arg, .. } => {
                if let Some(a) = arg {
                    self.check_expr(a);
                }
            }
            Stmt::Break { .. } | Stmt::Continue { .. } | Stmt::Empty(_) | Stmt::Debugger(_) => {}
        }
    }

    fn check_decl(&mut self, decl: &Decl) {
        self.check_decl_opts(decl, false)
    }

    fn check_decl_opts(&mut self, decl: &Decl, is_iteration_target: bool) {
        match decl {
            Decl::Var { kind, declarations, .. } => {
                for d in declarations {
                    // Declare the binding names with scope-aware conflict checks.
                    let mut names = Vec::new();
                    collect_binding_names(&d.name, &mut names);
                    let is_let = matches!(
                        kind,
                        js_syntax::ast::stmt::VarKind::Let
                            | js_syntax::ast::stmt::VarKind::Const
                            | js_syntax::ast::stmt::VarKind::Using
                            | js_syntax::ast::stmt::VarKind::AwaitUsing
                    );
                    for n in &names {
                        if is_let {
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
                    self.declare_var(name, f.span);
                }
                self.check_function(f, false, false, false, false);
            }
            Decl::Class(c) => {
                if let Some(name) = &c.name {
                    self.declare_lexical(name, c.span);
                }
                self.check_class(c);
            }
            Decl::Import { .. } | Decl::Export { .. } => {}
        }
    }

    fn check_function(
        &mut self,
        f: &FunctionDecl,
        force_strict: bool,
        super_prop_ok: bool,
        super_call_ok: bool,
        name_is_property: bool,
    ) {
        // Strictness: inherited, forced (class member/generator/async), or own
        // directive prologue.
        let body_strict = self.strict || force_strict || strict_directive_stmts(&f.body);
        // Function name (strict): eval/arguments/FRW restrictions. A *method*
        // name is a property name (IdentifierName), not a BindingIdentifier, so
        // it is exempt — `{ eval(){} }` / `class C { arguments(){} }` are fine.
        if body_strict && !name_is_property {
            if let Some(name) = &f.name {
                self.check_strict_binding_name(name, f.span);
            }
        }
        // Parameters.
        let non_simple = f.params.iter().any(|p| !matches!(p, Pat::Ident { .. }));
        // Enter a function scope so params + body lexical declarations interact
        // (e.g. a `let` in the body clashing with a parameter is an error).
        self.enter(true);
        self.declare_params(&f.params);
        self.check_param_list(&f.params, non_simple, body_strict);
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
    }

    /// Declare each parameter's binding names in the current (function) scope.
    /// Parameters behave like `var` (they coexist with `var`/other params but a
    /// `let`/`const`/`class` in the body clashes with one).
    fn declare_params(&mut self, params: &[Pat]) {
        for p in params {
            let mut names = Vec::new();
            collect_binding_names(p, &mut names);
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
        if let Some(sc) = &c.superclass {
            self.check_expr(sc);
        }
        let saved = self.strict;
        self.strict = true;
        let derived = c.superclass.is_some();
        let mut ctor_count = 0;
        for m in &c.body {
            use js_syntax::ast::expr::ClassMemberKind;
            match &m.value {
                js_syntax::ast::expr::ClassMemberValue::Method(func) => {
                    let is_ctor = matches!(m.kind, ClassMemberKind::Constructor);
                    if is_ctor {
                        ctor_count += 1;
                    }
                    // `super.x` is valid in any method/constructor; `super()`
                    // only in a constructor of a derived class.
                    let call_ok = is_ctor && derived;
                    self.check_function(func, true, true, call_ok, true);
                }
                js_syntax::ast::expr::ClassMemberValue::Field(init) => {
                    if let Some(e) = init {
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
                    self.super_prop.push(true);
                    self.super_call.push(false);
                    for s in body {
                        self.check_stmt(s);
                    }
                    self.super_prop.pop();
                    self.super_call.pop();
                }
            }
        }
        if ctor_count > 1 {
            self.err(c.span, "a class may not have more than one constructor");
        }
        self.strict = saved;
    }

    fn check_param_list(&mut self, params: &[Pat], non_simple: bool, strict: bool) {
        // Duplicate formal-parameter detection.
        let mut names: Vec<String> = Vec::new();
        for p in params {
            collect_binding_names(p, &mut names);
        }
        let mut seen = std::collections::HashSet::new();
        for n in &names {
            if !seen.insert(n.clone()) {
                // Duplicate. Always an error for non-simple parameter lists;
                // for simple lists, only in strict mode.
                if non_simple || strict {
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

    fn check_binding_pat(&mut self, pat: &Pat) {
        self.check_binding_pat_strict(pat, self.strict);
    }

    /// Walk a binding pattern, enforcing strict-mode name restrictions and the
    /// "rest element may not have a default" structural rule.
    fn check_binding_pat_strict(&mut self, pat: &Pat, strict: bool) {
        match pat {
            Pat::Ident { name, span } => {
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
                        ObjectPatProp::KeyValue { value, .. } => {
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
            Pat::Rest { arg, .. } => self.check_binding_pat_strict(arg, strict),
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
        if name == "eval" || name == "arguments" {
            self.err(span, format!("`{}` is not a valid binding name in strict mode", name));
            return;
        }
        if is_strict_future_reserved_word(name) {
            self.err(
                span,
                format!("`{}` is a reserved word in strict mode and cannot be a binding", name),
            );
        }
    }

    fn check_expr(&mut self, expr: &Expr) {
        match expr {
            Expr::Function(f) => self.check_function(f, false, false, false, false),
            Expr::Arrow(a) => self.check_arrow(a),
            Expr::Class(c) => self.check_class(c),
            Expr::Assign { op, left, right, .. } => {
                // Strict mode: `eval`/`arguments` are not assignment targets.
                if self.strict {
                    if let AssignTarget::Ident { name, span } = left {
                        if name == "eval" || name == "arguments" {
                            self.err(*span, format!("`{}` cannot be assigned in strict mode", name));
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
                    if let Expr::Ident { span, .. } = arg.as_ref() {
                        if self.strict {
                            self.err(*span, "`delete` of an unqualified identifier is not allowed in strict mode");
                        }
                    }
                }
                self.check_expr(arg);
            }
            Expr::Update { arg, span, .. } => {
                // `import(...)` and optional-chaining results are not valid
                // `++`/`--` operands.
                if matches!(arg.as_ref(), Expr::ImportCall { .. }) {
                    self.err(*span, "invalid update target: `import(...)` is not assignable");
                }
                if optional_chain_target(arg.as_ref()) {
                    self.err(*span, "invalid update target: an optional chaining is not assignable");
                }
                if self.strict {
                    if let Expr::Ident { name, span } = arg.as_ref() {
                        if name == "eval" || name == "arguments" {
                            self.err(*span, format!("`{}` cannot be updated in strict mode", name));
                        }
                    }
                }
                self.check_expr(arg);
            }
            Expr::Binary { left, right, .. }
            | Expr::Logical { left, right, .. } => {
                self.check_expr(left);
                self.check_expr(right);
            }
            Expr::Conditional { test, cons, alt, .. } => {
                self.check_expr(test);
                self.check_expr(cons);
                self.check_expr(alt);
            }
            Expr::Sequence { exprs, .. } => {
                for e in exprs {
                    self.check_expr(e);
                }
            }
            Expr::Member(m) => {
                // `super.x` / `super[expr]` require a valid super-property context.
                if matches!(m.object.as_ref(), Expr::Super(_)) && !*self.super_prop.last().unwrap() {
                    self.err(m.span, "`super` property is only valid in a class method or constructor");
                }
                self.check_expr(&m.object);
                if let js_syntax::ast::expr::MemberProp::Computed(e) = &m.property {
                    self.check_expr(e);
                }
            }
            Expr::Call(c) => {
                // `super(...)` requires a derived-class constructor context.
                if matches!(c.callee.as_ref(), Expr::Super(_)) && !*self.super_call.last().unwrap() {
                    self.err(c.span, "`super()` call is only valid in a derived class constructor");
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
                            self.check_function(f, false, true, false, true)
                        }
                        js_syntax::ast::expr::ObjectPropValue::Spread(e) => self.check_expr(e),
                    }
                }
                if proto_count > 1 {
                    self.err(expr.span(), "duplicate `__proto__` property in object literal");
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
            Expr::ImportCall { source, options, .. } => {
                self.check_expr(source);
                if let Some(o) = options {
                    self.check_expr(o);
                }
            }
            Expr::ImportMeta(_) => {}
            Expr::Yield { arg, span, .. } => {
                if !*self.yield_ctx.last().unwrap() {
                    self.err(*span, "`yield` is only valid within a generator function");
                }
                if let Some(a) = arg {
                    self.check_expr(a);
                }
            }
            Expr::Await { arg, span } => {
                if !*self.await_ctx.last().unwrap() {
                    self.err(*span, "`await` is only valid within async functions");
                }
                self.check_expr(arg);
            }
            Expr::Super(span) => {
                // Bare `super` must be a member access or call; both are handled
                // at the Member/Call level. A bare reference is invalid.
                if !*self.super_prop.last().unwrap() {
                    self.err(*span, "`super` is only valid in a class method or constructor");
                }
            }
            Expr::This(_)
            | Expr::Ident { .. }
            | Expr::Regex { .. } => {}
            Expr::Lit(lit) => {
                // Legacy octal (`077`, `010`) is a SyntaxError in strict mode.
                if self.strict {
                    if let Lit::Number(span, _, raw) = lit {
                        if is_legacy_octal(raw) {
                            self.err(*span, "legacy octal literals are not allowed in strict mode");
                        }
                    }
                }
            }
        }
    }

    fn check_assign_target(&mut self, target: &AssignTarget) {
        match target {
            AssignTarget::Pat(pat) => self.check_binding_pat(pat),
            AssignTarget::Member(m) => {
                // An optional-chaining member (`a?.b`) is not a valid assignment
                // target.
                if m.optional {
                    self.err(m.span, "invalid assignment target: an optional chaining is not assignable");
                }
                self.check_expr(&m.object);
            }
            AssignTarget::Ident { .. } => {}
        }
    }

    fn check_arrow(&mut self, a: &js_syntax::ast::expr::ArrowExpr) {
        // Arrow functions are strict iff their enclosing context is strict, or
        // (for a block body) they have their own directive prologue.
        let body_strict = match &a.body {
            ArrowBody::Block(stmts) => self.strict || strict_directive_stmts(stmts),
            ArrowBody::Expr(_) => self.strict,
        };
        let non_simple = a.params.iter().any(|p| !matches!(p, Pat::Ident { .. }));
        self.enter(true);
        self.declare_params(&a.params);
        self.check_param_list(&a.params, non_simple, body_strict);
        // Arrows inherit await-ness (own async OR enclosing); they are never
        // generators. They also inherit super context (transparent).
        let inherited_await = *self.await_ctx.last().unwrap();
        self.await_ctx.push(a.is_async || inherited_await);
        self.yield_ctx.push(false);
        let sp = *self.super_prop.last().unwrap();
        let sc = *self.super_call.last().unwrap();
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
    }
}

// ---- strict-mode determination helpers ----------------------------------

/// Whether a statement list begins with a `"use strict"` directive prologue.
fn strict_directive_stmts(body: &[Stmt]) -> bool {
    for s in body {
        match s {
            Stmt::Expr { expr, .. } => match directive_string(expr) {
                Some(d) if d == "use strict" => return true,
                Some(_) => continue, // other directives are still part of the prologue
                None => break,
            },
            _ => break,
        }
    }
    false
}

/// Top-level variant over [`ProgramItem`].
fn strict_directive_items(items: &[ProgramItem]) -> bool {
    for item in items {
        match item {
            ProgramItem::Stmt(Stmt::Expr { expr, .. }) => match directive_string(expr) {
                Some(d) if d == "use strict" => return true,
                Some(_) => continue,
                None => break,
            },
            _ => break,
        }
    }
    false
}

/// A directive is a string-literal expression statement (parens allowed).
fn directive_string(expr: &Expr) -> Option<String> {
    match expr {
        Expr::Lit(Lit::String(_, s)) => Some(s.clone()),
        Expr::Paren { expr, .. } => directive_string(expr),
        _ => None,
    }
}

/// Whether `e` is the result of an optional chaining (`a?.b`, `a?.[x]`,
/// `a?.()`), which is never a valid assignment / update target.
fn optional_chain_target(e: &Expr) -> bool {
    match e {
        Expr::Member(m) => m.optional,
        Expr::Call(c) => c.optional,
        _ => false,
    }
}

/// Collect all binding names introduced by a pattern (flattening nested
/// destructuring). Used for duplicate-parameter detection.
fn collect_binding_names(pat: &Pat, out: &mut Vec<String>) {
    match pat {
        Pat::Ident { name, .. } => out.push(name.clone()),
        Pat::Array { elements, .. } => {
            for el in elements.iter().flatten() {
                if let ArrayPatElement::Pat(p) = el {
                    collect_binding_names(p, out);
                }
            }
        }
        Pat::Object { properties, .. } => {
            for prop in properties {
                match prop {
                    ObjectPatProp::KeyValue { value, .. } => collect_binding_names(value, out),
                    ObjectPatProp::Rest { arg, .. } => collect_binding_names(arg, out),
                }
            }
        }
        Pat::Rest { arg, .. } => collect_binding_names(arg, out),
        Pat::Assignment { left, .. } => collect_binding_names(left, out),
        Pat::Member(_) => {}
    }
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
    use super::*;

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
        assert!(errs.iter().any(|m| m.contains("duplicate parameter")), "{:?}", errs);
    }

    #[test]
    fn duplicate_params_nonsimple() {
        // Non-simple (default) → duplicate always an error.
        let errs = check_src("function f(a, a = 1) {}");
        assert!(errs.iter().any(|m| m.contains("duplicate parameter")), "{:?}", errs);
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
    fn allows_sloppy_duplicates() {
        // Sloppy mode + simple params: duplicates allowed.
        let errs = check_src("function f(a, a) {}");
        assert!(errs.is_empty(), "{:?}", errs);
    }

    #[test]
    fn lexical_redeclaration() {
        let errs = check_src("let a; let a;");
        assert!(errs.iter().any(|m| m.contains("already been declared")), "{:?}", errs);
    }

    #[test]
    fn let_vs_var_clash() {
        let errs = check_src("let a; var a;");
        assert!(errs.iter().any(|m| m.contains("already been declared")), "{:?}", errs);
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
        assert!(errs.iter().any(|m| m.contains("already been declared")), "{:?}", errs);
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
