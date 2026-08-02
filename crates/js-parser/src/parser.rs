//! The parser entry point and main driver.
//!
//! Delegates statement parsing to [`crate::stmt`] and expression parsing to
//! [`crate::expr`]. The driver owns a [`ParseSess`] and a
//! [`ParserTokenStream`], and reports problems via [`js_diagnostics`].

use crate::sess::ParseSess;
use crate::stmt::StmtParser;
use crate::token_stream::ParserTokenStream;
use js_diagnostics::{DiagResult, DiagnosticPhase};
use js_syntax::ast::Program;
use js_syntax::ast::ProgramKind;
use js_syntax::source::SourceFile;
use js_syntax::Span;

/// The top-level parser. Borrows the [`ParseSess`] (its source text) for the
/// duration of the parse; the produced [`Program`] is fully owned.
pub struct Parser {
    tokens: ParserTokenStream,
}

impl Parser {
    pub fn new(sess: &ParseSess) -> Parser {
        Parser {
            tokens: ParserTokenStream::new(&sess.source.src),
        }
    }

    /// Parse a full program (script or module).
    pub fn parse(mut self, kind: ProgramKind) -> DiagResult<Program> {
        if matches!(kind, ProgramKind::Module) {
            self.tokens.set_module();
        }
        let start = self.tokens.span();
        let mut stmt = StmtParser::new(&mut self.tokens);
        let mut body = Vec::new();
        let mut errors = Vec::new();

        while !stmt.is_eof() {
            match stmt.parse_program_item() {
                Ok(item) => body.push(item),
                Err(mut diags) => {
                    for diagnostic in &mut diags {
                        diagnostic.classify(DiagnosticPhase::Parse, "JS-PARSE");
                    }
                    errors.extend(diags);
                    // Error recovery: advance past the failed statement so the
                    // loop always makes progress (otherwise a single bad token
                    // would spin forever).
                    stmt.recover_to_statement_boundary();
                }
            }
        }

        let end = stmt.span();
        if !errors.is_empty() {
            return Err(errors);
        }
        let program = Program::new(Span::new(start.start, end.end), kind, body);

        // Semantic early errors (strict-mode + spec constraints) — only worth
        // running once the syntactic parse succeeded cleanly.
        let mut early = crate::early_errors::check(&program);
        for diagnostic in &mut early {
            diagnostic.classify(DiagnosticPhase::EarlyError, "JS-EARLY");
        }
        if !early.is_empty() {
            return Err(early);
        }
        Ok(program)
    }
}

/// Parse `src` as a classic script.
pub fn parse(src: &str) -> DiagResult<Program> {
    parse_named("script", src, ProgramKind::Script)
}

/// Parse `src` as a script.
pub fn parse_script(src: &str) -> DiagResult<Program> {
    parse_named("script", src, ProgramKind::Script)
}

/// Parse `src` as a module.
pub fn parse_module(src: &str) -> DiagResult<Program> {
    parse_named("module", src, ProgramKind::Module)
}

fn parse_named(name: &str, src: &str, kind: ProgramKind) -> DiagResult<Program> {
    let source = SourceFile::new(name, std::sync::Arc::from(src));
    let sess = ParseSess::new(source);
    Parser::new(&sess).parse(kind)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_script_parses() {
        let prog = parse("").expect("empty source should parse");
        assert!(prog.body.is_empty());
    }

    #[test]
    fn private_names_parse() {
        // `#` private fields/methods/accessors, including async + generator.
        let src = "class C { #x = 1; #m() {} async #a() {} async *#g() {} get #y() {} }";
        parse(src).expect("private-name class body should parse");
    }

    #[test]
    fn class_fields_require_a_separator() {
        for src in [
            "class C { x y }",
            "class C { #x #y }",
            "class C { x m() {} }",
        ] {
            assert!(parse(src).is_err(), "{src}");
        }
        for src in ["class C { x\ny }", "class C { x; y }", "class C { x }"] {
            parse(src).unwrap_or_else(|errors| panic!("{src}: {errors:?}"));
        }
        parse("class C { accessor x; static accessor #y = 1; }")
            .expect("auto-accessor fields should parse");
    }

    #[test]
    fn class_heritage_requires_a_left_hand_side_expression() {
        assert!(parse("class C extends async () => {} {}").is_err());
        parse("class C extends (async () => {}) {}")
            .expect("a parenthesized arrow is a valid heritage expression");
    }

    #[test]
    fn class_modifiers_must_not_contain_escapes() {
        for src in [
            "class C { st\\u0061tic m() {} }",
            "class C { \\u0061sync m() {} }",
            "class C { \\u0061sync *m() {} }",
        ] {
            assert!(parse(src).is_err(), "{src}");
        }
        parse("class C { st\\u0061tic() {} \\u0061sync() {} }")
            .expect("escaped names remain valid ordinary property names");
    }

    #[test]
    fn dynamic_import_new_and_escape_boundaries() {
        for src in [
            "new import('x')",
            "new import('x').value",
            "new import.source('x').value",
            "im\\u0070ort('x')",
            "import.\\u0073ource('x')",
        ] {
            assert!(parse(src).is_err(), "{src}");
        }
        for src in ["new (import('x'))", "new (import('x').value)"] {
            parse(src).unwrap_or_else(|errors| panic!("{src}: {errors:?}"));
        }
    }

    #[test]
    fn escaped_keyword_and_async_function_boundaries() {
        for src in [
            "tru\\u{65};",
            "f\\u{61}lse;",
            "n\\u{75}ll;",
            "\\u0061sync function f() {}",
            "void \\u0061sync function f() {}",
            "async funct\\u0069on f() {}",
            "void async funct\\u0069on f() {}",
            "\\u0061sync function* f() {}",
            "void \\u0061sync function* f() {}",
            "async function f() { let\nawait 0; }",
        ] {
            assert!(parse(src).is_err(), "{src}");
        }

        for src in [
            "async function f() {}",
            "void async function f() {}",
            "async function* f() {}",
            "void async function* f() {}",
            "async\nfunction f() {}",
            "let\n{}",
            "if (false) let\nvalue = 1;",
            "l\\u0065t\nvalue; var value;",
        ] {
            parse(src).unwrap_or_else(|errors| panic!("{src}: {errors:?}"));
        }
    }

    #[test]
    fn static_binding_identifier_respects_strictness() {
        for src in [
            r#"var static = 1; var st\u0061tic = 2;"#,
            r#"{ let static = 1; } { const st\u0061tic = 2; }"#,
            r#"function f(static) { var static; }"#,
        ] {
            parse_script(src).unwrap_or_else(|errors| panic!("{src}: {errors:?}"));
        }

        for src in [
            r#"'use strict'; var static;"#,
            r#"function f() { 'use strict'; let st\u0061tic; }"#,
            r#"class C { method() { var static; } }"#,
        ] {
            assert!(parse_script(src).is_err(), "{src}");
        }
        assert!(parse_module("var static;").is_err());
    }

    #[test]
    fn conditional_in_parameter_propagation() {
        parse("for (true ? '' in {} : false; false; ) ;")
            .expect("the consequent of a conditional expression always permits `in`");
        assert!(parse("for (false ? true : '' in {}; false; ) ;").is_err());
    }

    #[test]
    fn yield_in_and_private_brand_check_boundaries() {
        for src in [
            "function* g() { for (yield '' in {}; ; ) ; }",
            "function* g() { for (yield * '' in {}; ; ) ; }",
            "class C { #x; m() { #x in #x in this; } }",
            "class C { #x; m() { for (#x in []) ; } }",
            "class C { #x; m() { #x; } }",
        ] {
            assert!(parse(src).is_err(), "{src}");
        }

        parse("class C { #x; m(object) { return #x in object; } }")
            .expect("a declared private name may be used in a private brand check");
    }

    #[test]
    fn lexical_let_binding_and_throw_line_terminator_boundaries() {
        for src in [
            "let\nlet;",
            "let\nlet = 1;",
            "const\nlet = 1;",
            "using let = null;",
            "throw\n1;",
            "throw;",
        ] {
            assert!(parse(src).is_err(), "{src}");
        }

        for src in ["var let;", "throw 1;", "function f() { return\n1; }"] {
            parse(src).unwrap_or_else(|errors| panic!("{src}: {errors:?}"));
        }
    }

    #[test]
    fn final_property_import_meta_and_delete_boundaries() {
        for src in [
            "object.\"property\";",
            "import.meta;",
            "'use strict'; delete ((identifier));",
        ] {
            assert!(parse_script(src).is_err(), "{src}");
        }

        for src in [
            "object.property; object['property'];",
            "import('module');",
            "delete ((identifier));",
            "'use strict'; delete object.property;",
        ] {
            parse_script(src).unwrap_or_else(|errors| panic!("{src}: {errors:?}"));
        }
        parse_module("import.meta;").expect("import.meta is valid with the Module goal");
    }

    #[test]
    fn object_literal_cover_and_method_boundaries() {
        assert_eq!(crate::expr::bigint_property_name("0x10n"), "16");
        assert_eq!(
            crate::expr::bigint_property_name("999_999_999_999_999_999n"),
            "999999999999999999"
        );
        for src in [
            "({ a = 1 });",
            "({ [key] });",
            "({ 0 });",
            "({ *name });",
            "({ get value(arg) {} });",
            "({ set value(...arg) {} });",
            "({ method(a, a) {} });",
            "({ async\nmethod() {} });",
            "({ \\u0061sync method() {} });",
            "({ g\\u0065t value() {} });",
            "({ #private() {} });",
        ] {
            assert!(parse(src).is_err(), "{src}");
        }
        for src in [
            "const f = ({ a = 1 }) => a;",
            "({ a = 1 } = source);",
            "({ 1n() {}, 999999999999999999n: true });",
            "class C { 1n() {} }",
            "({ async, method() {} });",
            "({ *g() { (function yield() {}); } });",
        ] {
            parse(src).unwrap_or_else(|errors| panic!("{src}: {errors:?}"));
        }
    }

    #[test]
    fn string_and_template_escape_boundaries() {
        for src in [
            r#""\u1";"#,
            r#""\xG0";"#,
            "\"unterminated",
            r#"`\x0`;"#,
            r#"`\u{110000}`;"#,
            r#"'use strict'; '\1';"#,
            "`unterminated",
        ] {
            assert!(parse(src).is_err(), "{src:?}");
        }
        for src in [
            r#"'\1';"#,
            r#"tag`\xG`;"#,
            r#"tag`left${value}\u{}`;"#,
            "'\0';",
            r#"`\0`;"#,
        ] {
            parse(src).unwrap_or_else(|errors| panic!("{src:?}: {errors:?}"));
        }
    }

    #[test]
    fn using_declaration_context_and_lookahead_boundaries() {
        for src in [
            "using resource = null;",
            "{ using [] = null; }",
            "{ using resource = null, {} = null; }",
            "switch (0) { case 0: using resource = null; }",
            "label: using resource = null;",
            "function f() { 'use strict'; { using x = null; var x; } }",
        ] {
            assert!(parse_script(src).is_err(), "{src}");
        }

        parse_module("using resource = null;").expect("module-level using is valid");
        for src in [
            "for (using x = null;;) break; for (using of = null;;) break;",
            "async function f() { for (await using of of []) {} }",
            "async function f() { var using = []; await using[0]; }",
            "class C { static { (() => { using await = null; }); } }",
        ] {
            parse_script(src).unwrap_or_else(|errors| panic!("{src}: {errors:?}"));
        }
    }

    #[test]
    fn return_and_labelled_statement_context_boundaries() {
        for src in [
            "return;",
            "{ return 1; }",
            "class C { static { return; } }",
            "label: let value;",
            "label: const value = 1;",
            "label: class C {}",
            "label: async function f() {}",
            "label: function* g() {}",
            "'use strict'; label: function f() {}",
            "'use strict'; yield: 0;",
            "class C { static { await: 0; } }",
            "if (false) label: function f() {}",
            "while (false) label: function f() {}",
            "with ({}) label: function f() {}",
            "with ({}) let value;",
            "with ({}) function f() {}",
        ] {
            assert!(parse_script(src).is_err(), "{src}");
        }
        assert!(parse_module("await: 0;").is_err());

        for src in [
            "function f() { return; }",
            "const f = () => { return 1; };",
            "label: var value;",
            "label: function f() {}",
            "yield: 0;",
            "await: 0;",
        ] {
            parse_script(src).unwrap_or_else(|errors| panic!("{src}: {errors:?}"));
        }
    }

    #[test]
    fn arrow_head_and_static_block_context_boundaries() {
        for src in [
            "enum => 0;",
            "({ \\u0065num }) => 0;",
            "const f = x\n=> x;",
            "const f = ()\n=> 0;",
            "async\n(foo) => {};",
            "\\u0061sync () => {};",
            "class C { static { (await => 0); } }",
            "class C { static { ((x = await) => 0); } }",
        ] {
            assert!(parse_script(src).is_err(), "{src:?}");
        }

        for src in [
            "const f = x =>\nx;",
            "const f = (x) =>\n{ return x; };",
            "\\u0061sync\np => {};",
            "({ enum: 1 }).enum;",
        ] {
            parse_script(src).unwrap_or_else(|errors| panic!("{src:?}: {errors:?}"));
        }
    }

    #[test]
    fn exponentiation_and_assignment_target_boundaries() {
        for src in [
            "-2 ** 2;",
            "typeof value ** 2;",
            "function f() { new.target = 1; }",
            "function f() { ++(new.target); }",
            "function* g() { (yield)++; }",
            "++this;",
            "call()--;",
            "({}) = value;",
            "() => ({}) = value;",
            "obj?.value.other++;",
            "n\\u0065w.target;",
            "function f() { new.t\\u0061rget; }",
        ] {
            assert!(parse_script(src).is_err(), "{src:?}");
        }

        for src in [
            "(-2) ** 2;",
            "++value; object.value--;",
            "(value) = 1; (object.value)++;",
            "({ value } = source);",
            "(obj?.value).other++;",
            "function f() { new.target; }",
        ] {
            parse_script(src).unwrap_or_else(|errors| panic!("{src:?}: {errors:?}"));
        }
    }

    #[test]
    fn numeric_coalesce_and_optional_chain_boundaries() {
        for src in [
            "3in [];",
            "0_1;",
            "08_0;",
            "a && b ?? c;",
            "a ?? b || c;",
            "object?.tag`value`;",
            "object?.tag\n`value`;",
        ] {
            assert!(parse_script(src).is_err(), "{src:?}");
        }

        for src in [
            "0x0_1; 0b0_1; 0o0_1; 0.1_2;",
            "(a && b) ?? c; a ?? (b || c);",
            "const value = true ?.30 : false;",
            "(object?.tag)`value`;",
        ] {
            parse_script(src).unwrap_or_else(|errors| panic!("{src:?}: {errors:?}"));
        }
    }

    #[test]
    fn for_in_of_heads_must_convert_to_assignment_patterns() {
        for src in [
            "for ((this) of []) {}",
            "for ([(x, y)] of []) {}",
            "for ({ x() {} } in obj) {}",
            "for ([...x, y] of []) {}",
            "for ([...x,] of []) {}",
            "for (x o\\u0066 []) {}",
            "for (async of []) {}",
        ] {
            assert!(parse(src).is_err(), "{src}");
        }
        for src in [
            "for (x of []) {}",
            "for (obj.x in source) {}",
            "for ([x, ...obj.rest] of values) {}",
            "for (var let of values) {}",
            "[...x,];",
            "for (\\u0061sync of []) {}",
            "for ((async) of []) {}",
            "for await (async of []) {}",
        ] {
            parse(src).unwrap_or_else(|errors| panic!("{src}: {errors:?}"));
        }
    }

    #[test]
    fn decorators_parse() {
        // Element-level + class-level decorators: id, member, private-member,
        // call, and the parenthesized form.
        let src = "\
function dec() {}
class C {
  @dec m() {}
  @ns.x @(dec) static #p = 1;
  @a.b.#c[args](1, 2) method() {}
}
@dec class D {}
var E = @dec class {};
";
        parse(src).expect("decorator syntax should parse");
    }

    #[test]
    fn member_target_destructuring_parses() {
        parse("[a.b] = x; ({ m: o.p } = y);").expect("member-target destructuring should parse");
    }

    #[test]
    fn import_export_parse() {
        let src = "\
import \"mod\";
import * as ns from \"mod\";
import { a, b as c } from \"mod\";
import def, { x } from \"mod\";
export var v = 1;
export function f() {}
export default 42;
export { a, c as cc };
export * from \"mod\";
export { a as other_a } from \"other\";
";
        parse_module(src).expect("module import/export should parse");
    }

    #[test]
    fn module_body_static_semantics_and_contextual_terminals() {
        for src in [
            "import def, * as ns from 'mod'; export { def }; export * as other from 'other';",
            "export default function() {}",
            "export default async function*() {}",
            "export default class {}",
            "export * from 'a'; export * from 'b';",
        ] {
            parse_module(src).unwrap_or_else(|errors| panic!("{src}: {errors:?}"));
        }

        for src in [
            r#"import { x, y as x } from 'mod';"#,
            r#"import def, * as def from 'mod';"#,
            r#"import { x as arguments } from 'mod';"#,
            r#"import { eval } from 'mod';"#,
            r#"import { x \u0061s y } from 'mod';"#,
            r#"import * \u0061s ns from 'mod';"#,
            r#"import {} \u0066rom 'mod';"#,
            r#"var x; export { x as z }; export * as z from 'mod';"#,
            r#"var x, y; export default x; export { y as default };"#,
            r#"var x, y; export { x as z }; export { y as z };"#,
            r#"export { Number };"#,
            r#"function f() {} function f() {}"#,
            r#"var f; function f() {}"#,
            r#"class F {} export default function F() {}"#,
            r#"export default function() {}();"#,
            r#"export default function*() {}();"#,
            r#"export d\u0065fault 0;"#,
            r#"export {} \u0066rom 'mod';"#,
            r#"\u0061wait 0;"#,
        ] {
            assert!(parse_module(src).is_err(), "{src}");
        }
    }

    #[test]
    fn module_requests_attributes_and_string_export_names() {
        let valid = r#"
import value from 'json' with { type: 'json' };
import * as ns from 'text'
with { type: 'text', "mode": "strict", };
import { "☿" as mercury, default as fallback } from 'names' with {};
import 'side-effect' with { if: '' };
export { mercury as "☿", fallback as fallbackExport };
export { "☿" as "planet" } from 'names' with { type: 'js' };
export * as "all names" from 'names' with {};
export default "ok";
"#;
        parse_module(valid).unwrap_or_else(|errors| panic!("{errors:?}"));
        parse_module(r#"export { "\uD83D\uDE00" } from 'names';"#)
            .expect("a paired surrogate ModuleExportName is well-formed");

        for src in [
            r#"import x from 'm' with { type: 'json', 'typ\u0065': '' };"#,
            r#"import 'm' with { type: 'json', type: '' };"#,
            r#"export * from 'm' with { type: 'json', "type": '' };"#,
            r#"import x from 'm' with { type: json };"#,
            r#"import { "name" } from 'm';"#,
            r#"export { "name" };"#,
            r#"export { "\uD83D" } from 'm';"#,
            r#"export * as "\uDE00" from 'm';"#,
            r#"import x from 'm' w\u0069th { type: 'json' };"#,
        ] {
            assert!(parse_module(src).is_err(), "{src}");
        }
    }

    #[test]
    fn static_deferred_import_phase_and_grammar_boundaries() {
        use js_syntax::ast::{Decl, ImportPhase, ImportSpec, ProgramItem};

        let deferred =
            parse_module(r#"import defer * as namespace from 'module' with { type: 'js' };"#)
                .expect("a deferred namespace import should parse");
        let ProgramItem::Decl(Decl::Import {
            spec: ImportSpec::Namespace { ns, request },
            ..
        }) = &deferred.body[0]
        else {
            panic!("expected a namespace import");
        };
        assert_eq!(ns, "namespace");
        assert_eq!(request.phase, ImportPhase::Defer);
        assert_eq!(request.specifier, "module");
        assert_eq!(request.attributes.len(), 1);

        let ordinary = parse_module(r#"import * as namespace from 'module';"#)
            .expect("an ordinary namespace import should parse");
        let ProgramItem::Decl(Decl::Import {
            spec: ImportSpec::Namespace { request, .. },
            ..
        }) = &ordinary.body[0]
        else {
            panic!("expected an ordinary namespace import");
        };
        assert_eq!(request.phase, ImportPhase::Eval);

        let default_defer = parse_module(r#"import defer from 'module';"#)
            .expect("`defer` remains a valid ordinary default binding");
        let ProgramItem::Decl(Decl::Import {
            spec: ImportSpec::Default { local, request, .. },
            ..
        }) = &default_defer.body[0]
        else {
            panic!("expected a default import");
        };
        assert_eq!(local, "defer");
        assert_eq!(request.phase, ImportPhase::Eval);

        parse_module("import defer\n* as namespace from 'module';")
            .expect("the deferred import production has no newline restriction");

        for src in [
            r#"import defer value from 'module';"#,
            r#"import defer { value } from 'module';"#,
            r#"import defer as namespace from 'module';"#,
            r#"import value, defer * as namespace from 'module';"#,
            r#"import defer value, * as namespace from 'module';"#,
            r#"export defer * as namespace from 'module';"#,
            r#"import d\u0065fer * as namespace from 'module';"#,
        ] {
            assert!(parse_module(src).is_err(), "{src}");
        }
    }

    #[test]
    fn let_disambiguation_parses() {
        // `let` as an identifier reference (sloppy mode), in for-heads and as
        // an expression statement. Each must parse cleanly.
        for src in [
            "for (let in obj) ;",
            "for (let; ;) break;",
            "let = 1; let;",
            "for (let.x of y) ;",
        ] {
            parse(src).unwrap_or_else(|e| panic!("`{src}` should parse: {}", e[0].message));
        }
    }
}
