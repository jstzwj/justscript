//! Reusable ECMAScript static-semantic queries.
//!
//! These functions mirror named specification operations. Early-error rules
//! compose them instead of duplicating syntax walks for individual productions.

use js_syntax::ast::lit::Lit;
use js_syntax::ast::pat::{ArrayPatElement, ObjectPatProp, Pat};
use js_syntax::ast::{
    expr::{ArrayExprElement, ArrowBody, AssignTarget, CallArg, Expr, MemberProp, ObjectPropValue},
    stmt::{Decl, ExportSpec, ForInit, ForTarget, ImportSpec, SwitchCase, VarKind},
    ProgramItem, Stmt,
};

pub(crate) fn bound_names(pat: &Pat) -> Vec<String> {
    let mut names = Vec::new();
    collect_bound_names(pat, &mut names);
    names
}

fn collect_bound_names(pat: &Pat, names: &mut Vec<String>) {
    match pat {
        Pat::Ident { name, .. } => names.push(name.clone()),
        Pat::Array { elements, .. } => {
            for element in elements.iter().flatten() {
                if let ArrayPatElement::Pat(pat) = element {
                    collect_bound_names(pat, names);
                }
            }
        }
        Pat::Object { properties, .. } => {
            for property in properties {
                match property {
                    ObjectPatProp::KeyValue { value, .. } => collect_bound_names(value, names),
                    ObjectPatProp::Rest { arg, .. } => collect_bound_names(arg, names),
                }
            }
        }
        Pat::Rest { arg, .. } => collect_bound_names(arg, names),
        Pat::Assignment { left, .. } => collect_bound_names(left, names),
        Pat::Member(_) => {}
    }
}

fn declaration_bound_names(decl: &Decl) -> Vec<String> {
    match decl {
        Decl::Var { declarations, .. } => declarations
            .iter()
            .flat_map(|declaration| bound_names(&declaration.name))
            .collect(),
        Decl::Function(function) => function.name.iter().cloned().collect(),
        Decl::Class(class) => class.name.iter().cloned().collect(),
        Decl::Import { spec, .. } => import_local_names(spec),
        Decl::Export { spec, .. } => match spec {
            ExportSpec::Decl(decl) | ExportSpec::DefaultDecl(decl) => declaration_bound_names(decl),
            _ => Vec::new(),
        },
    }
}

pub(crate) fn import_local_names(spec: &ImportSpec) -> Vec<String> {
    match spec {
        ImportSpec::Bare { .. } => Vec::new(),
        ImportSpec::Namespace { ns, .. } => vec![ns.clone()],
        ImportSpec::Named { items, .. } => items.iter().map(|item| item.local.clone()).collect(),
        ImportSpec::Default {
            local,
            namespace,
            named,
            ..
        } => {
            let mut names = vec![local.clone()];
            names.extend(namespace.iter().cloned());
            names.extend(named.iter().map(|item| item.local.clone()));
            names
        }
    }
}

/// Static-semantic sets attached to a ModuleBody. Lists are retained where the
/// specification checks duplicates; sets are used where only membership is
/// significant.
pub(crate) struct ModuleStaticSemantics {
    pub(crate) lexically_declared_names: Vec<String>,
    pub(crate) var_declared_names: std::collections::HashSet<String>,
    pub(crate) exported_names: Vec<String>,
    pub(crate) exported_bindings: Vec<String>,
    pub(crate) imported_local_names: Vec<String>,
    pub(crate) invalid_local_export_names: Vec<String>,
    pub(crate) duplicate_attribute_keys: Vec<(js_syntax::Span, String)>,
}

pub(crate) fn module_static_semantics(body: &[ProgramItem]) -> ModuleStaticSemantics {
    let mut lexically_declared_names = Vec::new();
    let mut var_declared_names = std::collections::HashSet::new();
    let mut exported_names = Vec::new();
    let mut exported_bindings = Vec::new();
    let mut imported_local_names = Vec::new();
    let mut invalid_local_export_names = Vec::new();
    let mut duplicate_attribute_keys = Vec::new();

    for item in body {
        match item {
            ProgramItem::Stmt(stmt) => {
                collect_direct_module_lexical_names(stmt, &mut lexically_declared_names);
                collect_var_declared_names(stmt, &mut var_declared_names);
            }
            ProgramItem::Decl(decl) => {
                collect_module_declared_names(
                    decl,
                    &mut lexically_declared_names,
                    &mut var_declared_names,
                    &mut imported_local_names,
                );
                collect_module_exports(decl, &mut exported_names, &mut exported_bindings);
                collect_module_request_errors(decl, &mut duplicate_attribute_keys);
                if let Decl::Export {
                    spec: ExportSpec::Named { items },
                    ..
                } = decl
                {
                    invalid_local_export_names.extend(
                        items
                            .iter()
                            .filter(|item| item.local.is_string())
                            .map(|item| item.local.value().to_string()),
                    );
                }
            }
        }
    }

    ModuleStaticSemantics {
        lexically_declared_names,
        var_declared_names,
        exported_names,
        exported_bindings,
        imported_local_names,
        invalid_local_export_names,
        duplicate_attribute_keys,
    }
}

fn collect_module_request_errors(decl: &Decl, duplicates: &mut Vec<(js_syntax::Span, String)>) {
    let request = match decl {
        Decl::Import { spec, .. } => Some(match spec {
            ImportSpec::Bare { request }
            | ImportSpec::Namespace { request, .. }
            | ImportSpec::Named { request, .. }
            | ImportSpec::Default { request, .. } => request,
        }),
        Decl::Export { spec, .. } => match spec {
            ExportSpec::All { request, .. } | ExportSpec::ReExport { request, .. } => Some(request),
            _ => None,
        },
        _ => None,
    };
    let Some(request) = request else {
        return;
    };
    let mut seen = std::collections::HashSet::new();
    for attribute in &request.attributes {
        if !seen.insert(attribute.key.clone()) {
            duplicates.push((attribute.span, attribute.key.clone()));
        }
    }
}

fn collect_direct_module_lexical_names(stmt: &Stmt, names: &mut Vec<String>) {
    let mut lexical = Vec::new();
    collect_direct_lexically_declared_names(stmt, &mut lexical);
    names.extend(lexical.into_iter().map(|entry| entry.name));
}

fn collect_module_declared_names(
    decl: &Decl,
    lexical: &mut Vec<String>,
    vars: &mut std::collections::HashSet<String>,
    imports: &mut Vec<String>,
) {
    match decl {
        Decl::Var {
            kind, declarations, ..
        } => {
            let names = declarations
                .iter()
                .flat_map(|declaration| bound_names(&declaration.name));
            if *kind == VarKind::Var {
                vars.extend(names);
            } else {
                lexical.extend(names);
            }
        }
        Decl::Function(function) => lexical.extend(function.name.iter().cloned()),
        Decl::Class(class) => lexical.extend(class.name.iter().cloned()),
        Decl::Import { spec, .. } => {
            let names = import_local_names(spec);
            lexical.extend(names.iter().cloned());
            imports.extend(names);
        }
        Decl::Export { spec, .. } => match spec {
            ExportSpec::Decl(decl) => collect_module_declared_names(decl, lexical, vars, imports),
            ExportSpec::DefaultDecl(decl) => {
                let names = declaration_bound_names(decl);
                if names.is_empty() {
                    lexical.push("*default*".to_string());
                } else {
                    lexical.extend(names);
                }
            }
            ExportSpec::Default(_) => lexical.push("*default*".to_string()),
            ExportSpec::Named { .. } | ExportSpec::All { .. } | ExportSpec::ReExport { .. } => {}
        },
    }
}

fn collect_module_exports(
    decl: &Decl,
    exported_names: &mut Vec<String>,
    exported_bindings: &mut Vec<String>,
) {
    let Decl::Export { spec, .. } = decl else {
        return;
    };
    match spec {
        ExportSpec::Named { items } => {
            exported_names.extend(items.iter().map(|item| item.exported.value().to_string()));
            exported_bindings.extend(
                items
                    .iter()
                    .filter(|item| !item.local.is_string())
                    .map(|item| item.local.value().to_string()),
            );
        }
        ExportSpec::Default(_) => {
            exported_names.push("default".to_string());
            exported_bindings.push("*default*".to_string());
        }
        ExportSpec::DefaultDecl(decl) => {
            exported_names.push("default".to_string());
            let names = declaration_bound_names(decl);
            if names.is_empty() {
                exported_bindings.push("*default*".to_string());
            } else {
                exported_bindings.extend(names);
            }
        }
        ExportSpec::All {
            exported: Some(name),
            ..
        } => exported_names.push(name.value().to_string()),
        ExportSpec::All { exported: None, .. } => {}
        ExportSpec::ReExport { items, .. } => {
            exported_names.extend(items.iter().map(|item| item.exported.value().to_string()));
        }
        ExportSpec::Decl(decl) => {
            let names = declaration_bound_names(decl);
            exported_names.extend(names.iter().cloned());
            exported_bindings.extend(names);
        }
    }
}

pub(crate) fn is_simple_parameter_list(params: &[Pat]) -> bool {
    params.iter().all(|p| matches!(p, Pat::Ident { .. }))
}

/// One entry in the LexicallyDeclaredNames of a StatementList. Ordinary
/// functions are tagged because Annex B permits duplicate ordinary function
/// declarations in sloppy blocks; no other lexical duplicate is legal.
pub(crate) struct LexicalName {
    pub(crate) name: String,
    pub(crate) ordinary_function: bool,
}

pub(crate) fn statement_list_lexically_declared_names(statements: &[Stmt]) -> Vec<LexicalName> {
    let mut names = Vec::new();
    for stmt in statements {
        collect_direct_lexically_declared_names(stmt, &mut names);
    }
    names
}

pub(crate) fn statement_list_var_declared_names(
    statements: &[Stmt],
) -> std::collections::HashSet<String> {
    let mut names = std::collections::HashSet::new();
    for stmt in statements {
        collect_var_declared_names(stmt, &mut names);
    }
    names
}

fn collect_direct_lexically_declared_names(stmt: &Stmt, names: &mut Vec<LexicalName>) {
    let Stmt::Decl(decl) = stmt else {
        return;
    };
    match decl.as_ref() {
        Decl::Var {
            kind, declarations, ..
        } if *kind != VarKind::Var => {
            for declaration in declarations {
                names.extend(
                    bound_names(&declaration.name)
                        .into_iter()
                        .map(|name| LexicalName {
                            name,
                            ordinary_function: false,
                        }),
                );
            }
        }
        Decl::Function(function) => {
            if let Some(name) = &function.name {
                names.push(LexicalName {
                    name: name.clone(),
                    ordinary_function: !function.is_async && !function.is_generator,
                });
            }
        }
        Decl::Class(class) => {
            if let Some(name) = &class.name {
                names.push(LexicalName {
                    name: name.clone(),
                    ordinary_function: false,
                });
            }
        }
        _ => {}
    }
}

pub(crate) fn switch_lexically_declared_names(cases: &[SwitchCase]) -> Vec<LexicalName> {
    let mut names = Vec::new();
    for stmt in cases.iter().flat_map(|case| &case.body) {
        collect_direct_lexically_declared_names(stmt, &mut names);
    }
    names
}

pub(crate) fn switch_var_declared_names(cases: &[SwitchCase]) -> std::collections::HashSet<String> {
    let mut names = std::collections::HashSet::new();
    for stmt in cases.iter().flat_map(|case| &case.body) {
        collect_var_declared_names(stmt, &mut names);
    }
    names
}

pub(crate) fn var_declared_names(stmt: &Stmt) -> std::collections::HashSet<String> {
    let mut names = std::collections::HashSet::new();
    collect_var_declared_names(stmt, &mut names);
    names
}

fn collect_var_declared_names(stmt: &Stmt, names: &mut std::collections::HashSet<String>) {
    match stmt {
        Stmt::Decl(decl) => {
            if let Decl::Var {
                kind: VarKind::Var,
                declarations,
                ..
            } = decl.as_ref()
            {
                for declaration in declarations {
                    names.extend(bound_names(&declaration.name));
                }
            }
        }
        Stmt::Block { body, .. } => {
            for stmt in body {
                collect_var_declared_names(stmt, names);
            }
        }
        Stmt::If { cons, alt, .. } => {
            collect_var_declared_names(cons, names);
            if let Some(alt) = alt {
                collect_var_declared_names(alt, names);
            }
        }
        Stmt::Switch { cases, .. } => {
            for stmt in cases.iter().flat_map(|case| &case.body) {
                collect_var_declared_names(stmt, names);
            }
        }
        Stmt::Try {
            block,
            handler,
            finalizer,
            ..
        } => {
            for stmt in &block.body {
                collect_var_declared_names(stmt, names);
            }
            if let Some(handler) = handler {
                for stmt in &handler.body {
                    collect_var_declared_names(stmt, names);
                }
            }
            if let Some(finalizer) = finalizer {
                for stmt in finalizer {
                    collect_var_declared_names(stmt, names);
                }
            }
        }
        Stmt::While { body, .. }
        | Stmt::DoWhile { body, .. }
        | Stmt::Labeled { body, .. }
        | Stmt::With { body, .. } => collect_var_declared_names(body, names),
        Stmt::For { init, body, .. } => {
            if let Some(ForInit::Var(decl)) = init {
                if let Decl::Var {
                    kind: VarKind::Var,
                    declarations,
                    ..
                } = decl.as_ref()
                {
                    for declaration in declarations {
                        names.extend(bound_names(&declaration.name));
                    }
                }
            }
            collect_var_declared_names(body, names);
        }
        Stmt::ForIn { left, body, .. } | Stmt::ForOf { left, body, .. } => {
            if let ForTarget::Var(decl) = left {
                if let Decl::Var {
                    kind: VarKind::Var,
                    declarations,
                    ..
                } = decl.as_ref()
                {
                    for declaration in declarations {
                        names.extend(bound_names(&declaration.name));
                    }
                }
            }
            collect_var_declared_names(body, names);
        }
        Stmt::Empty(_)
        | Stmt::Debugger(_)
        | Stmt::Expr { .. }
        | Stmt::Return { .. }
        | Stmt::Break { .. }
        | Stmt::Continue { .. }
        | Stmt::Throw { .. } => {}
    }
}

pub(crate) fn contains_use_strict(body: &[Stmt]) -> bool {
    body.iter().map_while(directive).any(|d| d == "use strict")
}

pub(crate) fn program_contains_use_strict(body: &[ProgramItem]) -> bool {
    body.iter()
        .map_while(|item| match item {
            ProgramItem::Stmt(stmt) => directive(stmt),
            ProgramItem::Decl(_) => None,
        })
        .any(|d| d == "use strict")
}

fn directive(stmt: &Stmt) -> Option<&str> {
    match stmt {
        Stmt::Expr { expr, .. } => match expr.as_ref() {
            js_syntax::ast::Expr::Lit(Lit::String(_, value, _)) => Some(value.as_str()),
            _ => None,
        },
        _ => None,
    }
}

/// The `ContainsArguments` query used by class field initializers. Arrow
/// functions are transparent; ordinary functions and nested classes introduce
/// their own `arguments` context and stop the walk.
pub(crate) fn contains_arguments(expr: &Expr) -> bool {
    match expr {
        Expr::Ident { name, .. } => name == "arguments",
        Expr::This(_)
        | Expr::Super(_)
        | Expr::NewTarget(_)
        | Expr::Lit(_)
        | Expr::Regex { .. }
        | Expr::ImportMeta(_) => false,
        Expr::Function(_) => false,
        Expr::Class(class) => {
            class.superclass.as_deref().is_some_and(contains_arguments)
                || class.decorators.iter().any(contains_arguments)
                || class.body.iter().any(|member| {
                    member.decorators.iter().any(contains_arguments)
                        || prop_key_contains_arguments(&member.key)
                })
        }
        Expr::Arrow(arrow) => {
            arrow.params.iter().any(pattern_contains_arguments)
                || match &arrow.body {
                    ArrowBody::Block(body) => body.iter().any(statement_contains_arguments),
                    ArrowBody::Expr(expr) => contains_arguments(expr),
                }
        }
        Expr::TemplateLit { expressions, .. }
        | Expr::Sequence {
            exprs: expressions, ..
        } => expressions.iter().any(contains_arguments),
        Expr::Array { elements, .. } => elements.iter().flatten().any(|element| match element {
            ArrayExprElement::Expr(expr) | ArrayExprElement::Spread(expr) => {
                contains_arguments(expr)
            }
        }),
        Expr::Object { props, .. } => props.iter().any(|prop| {
            prop_key_contains_arguments(&prop.key)
                || match &prop.value {
                    ObjectPropValue::Expr(expr) | ObjectPropValue::Spread(expr) => {
                        contains_arguments(expr)
                    }
                    ObjectPropValue::Method(_) => false,
                }
        }),
        Expr::Paren { expr, .. }
        | Expr::Unary { arg: expr, .. }
        | Expr::Update { arg: expr, .. }
        | Expr::Spread { arg: expr, .. }
        | Expr::Await { arg: expr, .. } => contains_arguments(expr),
        Expr::Binary { left, right, .. } | Expr::Logical { left, right, .. } => {
            contains_arguments(left) || contains_arguments(right)
        }
        Expr::PrivateIn { right, .. } => contains_arguments(right),
        Expr::Conditional {
            test, cons, alt, ..
        } => contains_arguments(test) || contains_arguments(cons) || contains_arguments(alt),
        Expr::Assign { left, right, .. } => {
            assign_target_contains_arguments(left) || contains_arguments(right)
        }
        Expr::Member(member) => {
            contains_arguments(&member.object)
                || matches!(&member.property, MemberProp::Computed(expr) if contains_arguments(expr))
        }
        Expr::Call(call) => {
            contains_arguments(&call.callee) || call.args.iter().any(call_arg_contains_arguments)
        }
        Expr::New(new) => {
            contains_arguments(&new.callee) || new.args.iter().any(call_arg_contains_arguments)
        }
        Expr::TaggedTemplate { tag, template, .. } => {
            contains_arguments(tag) || contains_arguments(template)
        }
        Expr::Yield { arg, .. } => arg.as_deref().is_some_and(contains_arguments),
        Expr::ImportCall {
            source, options, ..
        } => contains_arguments(source) || options.as_deref().is_some_and(contains_arguments),
    }
}

fn call_arg_contains_arguments(arg: &CallArg) -> bool {
    match arg {
        CallArg::Expr(expr) | CallArg::Spread(expr) => contains_arguments(expr),
    }
}

fn assign_target_contains_arguments(target: &AssignTarget) -> bool {
    match target {
        AssignTarget::Ident { name, .. } => name == "arguments",
        AssignTarget::Member(member) => {
            contains_arguments(&member.object)
                || matches!(&member.property, MemberProp::Computed(expr) if contains_arguments(expr))
        }
        AssignTarget::Pat(pat) => pattern_contains_arguments(pat),
    }
}

fn prop_key_contains_arguments(key: &js_syntax::ast::pat::PropKey) -> bool {
    matches!(key, js_syntax::ast::pat::PropKey::Computed(expr) if contains_arguments(expr))
}

fn pattern_contains_arguments(pat: &Pat) -> bool {
    match pat {
        Pat::Ident { .. } => false,
        Pat::Array { elements, .. } => elements.iter().flatten().any(|element| match element {
            ArrayPatElement::Pat(pat) => pattern_contains_arguments(pat),
            ArrayPatElement::Hole(_) => false,
        }),
        Pat::Object { properties, .. } => properties.iter().any(|property| match property {
            ObjectPatProp::KeyValue { key, value, .. } => {
                prop_key_contains_arguments(key) || pattern_contains_arguments(value)
            }
            ObjectPatProp::Rest { arg, .. } => pattern_contains_arguments(arg),
        }),
        Pat::Rest { arg, .. } => pattern_contains_arguments(arg),
        Pat::Assignment { left, right, .. } => {
            pattern_contains_arguments(left) || contains_arguments(right)
        }
        Pat::Member(member) => {
            contains_arguments(&member.object)
                || matches!(&member.property, MemberProp::Computed(expr) if contains_arguments(expr))
        }
    }
}

fn statement_contains_arguments(stmt: &Stmt) -> bool {
    match stmt {
        Stmt::Block { body, .. } => body.iter().any(statement_contains_arguments),
        Stmt::Empty(_) | Stmt::Debugger(_) | Stmt::Break { .. } | Stmt::Continue { .. } => false,
        Stmt::Expr { expr, .. } => contains_arguments(expr),
        Stmt::Decl(decl) => declaration_contains_arguments(decl),
        Stmt::If {
            test, cons, alt, ..
        } => {
            contains_arguments(test)
                || statement_contains_arguments(cons)
                || alt.as_deref().is_some_and(statement_contains_arguments)
        }
        Stmt::Switch { disc, cases, .. } => {
            contains_arguments(disc)
                || cases.iter().any(|case| {
                    case.test.as_ref().is_some_and(contains_arguments)
                        || case.body.iter().any(statement_contains_arguments)
                })
        }
        Stmt::Return { arg, .. } => arg.as_deref().is_some_and(contains_arguments),
        Stmt::Throw { arg, .. } => contains_arguments(arg),
        Stmt::Try {
            block,
            handler,
            finalizer,
            ..
        } => {
            block.body.iter().any(statement_contains_arguments)
                || handler.as_ref().is_some_and(|handler| {
                    handler
                        .param
                        .as_ref()
                        .is_some_and(pattern_contains_arguments)
                        || handler.body.iter().any(statement_contains_arguments)
                })
                || finalizer
                    .as_ref()
                    .is_some_and(|body| body.iter().any(statement_contains_arguments))
        }
        Stmt::While { test, body, .. } | Stmt::DoWhile { test, body, .. } => {
            contains_arguments(test) || statement_contains_arguments(body)
        }
        Stmt::For {
            init,
            test,
            update,
            body,
            ..
        } => {
            init.as_ref().is_some_and(|init| match init {
                ForInit::Var(decl) => declaration_contains_arguments(decl),
                ForInit::Expr(expr) => contains_arguments(expr),
            }) || test.as_deref().is_some_and(contains_arguments)
                || update.as_deref().is_some_and(contains_arguments)
                || statement_contains_arguments(body)
        }
        Stmt::ForIn {
            left, right, body, ..
        }
        | Stmt::ForOf {
            left, right, body, ..
        } => {
            (match left {
                ForTarget::Var(decl) => declaration_contains_arguments(decl),
                ForTarget::Pat(pat) => pattern_contains_arguments(pat),
            }) || contains_arguments(right)
                || statement_contains_arguments(body)
        }
        Stmt::Labeled { body, .. } => statement_contains_arguments(body),
        Stmt::With { obj, body, .. } => {
            contains_arguments(obj) || statement_contains_arguments(body)
        }
    }
}

pub(crate) fn statements_contain_arguments(statements: &[Stmt]) -> bool {
    statements.iter().any(statement_contains_arguments)
}

fn declaration_contains_arguments(decl: &Decl) -> bool {
    match decl {
        Decl::Var { declarations, .. } => declarations.iter().any(|declaration| {
            pattern_contains_arguments(&declaration.name)
                || declaration.init.as_ref().is_some_and(contains_arguments)
        }),
        Decl::Function(_) | Decl::Class(_) | Decl::Import { .. } => false,
        Decl::Export { spec, .. } => match spec {
            ExportSpec::Default(expr) => contains_arguments(expr),
            ExportSpec::DefaultDecl(decl) | ExportSpec::Decl(decl) => {
                declaration_contains_arguments(decl)
            }
            _ => false,
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use js_syntax::{ast::Expr, Span};

    fn string_stmt(value: &str) -> Stmt {
        Stmt::Expr {
            span: Span::DUMMY,
            expr: Box::new(Expr::Lit(Lit::String(Span::DUMMY, value.into(), false))),
        }
    }

    #[test]
    fn use_strict_must_be_in_the_directive_prologue() {
        assert!(contains_use_strict(&[
            string_stmt("other"),
            string_stmt("use strict")
        ]));
        assert!(!contains_use_strict(&[
            Stmt::Empty(Span::DUMMY),
            string_stmt("use strict"),
        ]));
    }

    #[test]
    fn parenthesized_string_is_not_a_directive() {
        let stmt = Stmt::Expr {
            span: Span::DUMMY,
            expr: Box::new(Expr::Paren {
                span: Span::DUMMY,
                expr: Box::new(Expr::Lit(Lit::String(
                    Span::DUMMY,
                    "use strict".into(),
                    false,
                ))),
            }),
        };
        assert!(!contains_use_strict(&[stmt]));
    }
}
