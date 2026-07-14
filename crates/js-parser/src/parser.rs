//! The parser entry point and main driver.
//!
//! Delegates statement parsing to [`crate::stmt`] and expression parsing to
//! [`crate::expr`]. The driver owns a [`ParseSess`] and a
//! [`ParserTokenStream`], and reports problems via [`js_diagnostics`].

use crate::sess::ParseSess;
use crate::stmt::StmtParser;
use crate::token_stream::ParserTokenStream;
use js_diagnostics::DiagResult;
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
                Err(diags) => {
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
        let early = crate::early_errors::check(&program);
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
export { a, b as bb };
export * from \"mod\";
export { a } from \"other\";
";
        parse(src).expect("module import/export should parse");
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
