//! Parser-level acceptance for the source-phase import cluster.
//!
//! The source-phase Test262 fixtures are inlined here so the test is hermetic:
//! it does not read the (gitignored, locally-pinned) `test262/test262` checkout,
//! and therefore passes on a clean checkout / in CI. The runtime behaviour
//! (linking + ModuleSource) lives in `js-engine`; this test only asserts that
//! every involved source text PARSES and that the source-phase `ModuleRequest`
//! carries `ImportPhase::Source`.
//!
//! The inlined sources are the minimal Module bodies the Test262 fixtures use;
//! the `import source`/`export` shapes are reproduced verbatim.

use js_parser::parse_module;
use js_syntax::ast::{Decl, ExportSpec, ImportPhase, ImportSpec, ProgramItem};

/// Parse a module source, panicking with the source on any failure.
fn parse(src: &str) -> Vec<ProgramItem> {
    parse_module(src)
        .unwrap_or_else(|errors| panic!("parse failed for {src:?}: {errors:?}"))
        .body
}

// `source-phase-import/reexport-source-binding_FIXTURE.js`
const REEXPORT_FIXTURE: &str = "import source x from '<module source>';\nexport { x };\n";

// `ambiguous-export-bindings/namespace-import-source-and-export-reexport_FIXTURE.js`
const AGGREGATOR_FIXTURE: &str = "export * from './namespace-import-source-and-export-1_FIXTURE.js';\nexport * from './namespace-import-source-and-export-2_FIXTURE.js';\n";

// Both leaf fixtures are identical: `import source mod from '<module source>'; export { mod };`
const LEAF_FIXTURE: &str = "import source mod from '<module source>';\nexport { mod };\n";

// `source-phase-import/import-source-binding-name_FIXTURE.js`
//   - `import source from '...'` and `import from from '...'` — ordinary
//     EVAL-phase default imports whose local names are the contextual keywords
//     `source` and `from`. The side-effect import is part of the fixture but is
//     irrelevant to the source-phase assertions.
const BINDING_NAME_EVAL_FIXTURE: &str =
    "import \"./ensure-linking-error_FIXTURE.js\";\nimport source from '<do not resolve>';\nimport from from '<do not resolve>';\n";

// `source-phase-import/import-source-binding-name-2_FIXTURE.js`
//   - `import source source from '...'` and `import source from from '...'` —
//     SOURCE-phase imports whose bindings are themselves named `source`/`from`.
const BINDING_NAME_SOURCE_FIXTURE: &str =
    "import \"./ensure-linking-error_FIXTURE.js\";\nimport source source from '<do not resolve>';\nimport source from from '<do not resolve>';\n";

// `source-phase-import/import-source-newlines_FIXTURE.js`
//   Exercises line terminators between every terminal of the production.
const NEWLINES_FIXTURE: &str =
    "import \"./ensure-linking-error_FIXTURE.js\";\nimport\n\n source\n\n y from '<do not resolve>';\n";

/// Find the first source-phase import in a list of program items and assert
/// its shape, returning the (local, specifier) pair.
fn expect_source_phase_import(items: &[ProgramItem]) -> (&str, &str) {
    for item in items {
        if let ProgramItem::Decl(Decl::Import {
            spec: ImportSpec::Default { local, request, .. },
            ..
        }) = item
        {
            if request.phase == ImportPhase::Source {
                return (local.as_str(), request.specifier.as_str());
            }
        }
    }
    panic!("no source-phase import found in fixture; items = {items:?}");
}

#[test]
fn reexport_source_binding_named_import_parses() {
    // `import source x from '<module source>'; export { x };`
    let items = parse(REEXPORT_FIXTURE);
    let (local, specifier) = expect_source_phase_import(&items);
    assert_eq!(local, "x");
    assert_eq!(
        specifier, "<module source>",
        "the Test262 virtual source-phase specifier must be preserved verbatim"
    );
    // The trailing `export { x }` is a local named export (the runtime
    // reclassifies it to an indirect ~source~ re-export).
    let has_reexport = items.iter().any(|item| {
        matches!(
            item,
            ProgramItem::Decl(Decl::Export {
                spec: ExportSpec::Named { .. },
                ..
            })
        )
    });
    assert!(has_reexport, "fixture should re-export the source binding");
}

#[test]
fn reexport_source_binding_namespace_get_parses() {
    let items = parse(REEXPORT_FIXTURE);
    let (local, specifier) = expect_source_phase_import(&items);
    assert_eq!(local, "x");
    assert_eq!(specifier, "<module source>");
}

#[test]
fn namespace_unambiguous_if_import_source_and_export_parses() {
    // The re-export aggregator: two star re-exports from leaf fixtures.
    let reexport = parse(AGGREGATOR_FIXTURE);
    let star_count = reexport
        .iter()
        .filter(|item| {
            matches!(
                item,
                ProgramItem::Decl(Decl::Export {
                    spec: ExportSpec::All { .. },
                    ..
                })
            )
        })
        .count();
    assert_eq!(
        star_count, 2,
        "aggregator should star-re-export from both leaf fixtures"
    );

    // Both leaf fixtures contain `import source mod from '<module source>';
    // export { mod };` and must emit phase Source.
    for src in [LEAF_FIXTURE, LEAF_FIXTURE] {
        let items = parse(src);
        let (local, specifier) = expect_source_phase_import(&items);
        assert_eq!(local, "mod");
        assert_eq!(specifier, "<module source>");
    }
}

#[test]
fn import_source_binding_name_fixtures_parse() {
    // Eval-phase default imports whose locals are the contextual keywords
    // `source` and `from`.
    let eval_items = parse(BINDING_NAME_EVAL_FIXTURE);
    let eval_defaults: Vec<(String, ImportPhase)> = eval_items
        .iter()
        .filter_map(|item| match item {
            ProgramItem::Decl(Decl::Import {
                spec: ImportSpec::Default { local, request, .. },
                ..
            }) => Some((local.clone(), request.phase)),
            _ => None,
        })
        .collect();
    assert!(
        eval_defaults
            .iter()
            .any(|(n, p)| n == "source" && *p == ImportPhase::Eval),
        "eval-phase default `source` binding missing: {eval_defaults:?}"
    );
    assert!(
        eval_defaults
            .iter()
            .any(|(n, p)| n == "from" && *p == ImportPhase::Eval),
        "eval-phase default `from` binding missing: {eval_defaults:?}"
    );

    // Source-phase imports whose bindings are themselves named `source`/`from`.
    let source_items = parse(BINDING_NAME_SOURCE_FIXTURE);
    let source_defaults: Vec<(&str, ImportPhase)> = source_items
        .iter()
        .filter_map(|item| match item {
            ProgramItem::Decl(Decl::Import {
                spec: ImportSpec::Default { local, request, .. },
                ..
            }) => Some((local.as_str(), request.phase)),
            _ => None,
        })
        .collect();
    assert!(
        source_defaults
            .iter()
            .any(|(n, p)| *n == "source" && *p == ImportPhase::Source),
        "source-phase import with binding `source` missing: {source_defaults:?}"
    );
    assert!(
        source_defaults
            .iter()
            .any(|(n, p)| *n == "from" && *p == ImportPhase::Source),
        "source-phase import with binding `from` missing: {source_defaults:?}"
    );
}

#[test]
fn import_source_newlines_fixture_parses() {
    // Exercises newlines between every terminal of the source-phase production.
    let items = parse(NEWLINES_FIXTURE);
    let (local, specifier) = expect_source_phase_import(&items);
    assert_eq!(local, "y");
    assert_eq!(specifier, "<do not resolve>");
}
