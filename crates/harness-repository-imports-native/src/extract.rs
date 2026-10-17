//! C and C++ include extraction: `#include "…"` and `#include <…>`, wherever the directive sits
//! (conditional blocks included, since which branch is taken is not decided here).
use harness_workspace::{
    import_graph::{
        ByteSpan, ExtractedFile, FactBuilder, ImportForm, ImportLimits, LanguageImportAdapter,
        LanguageProfile, Staticness,
    },
    repository_intelligence::RootRelativePath,
};
use tree_sitter::Node;

pub const ADAPTER_VERSION: u32 = 1;

/// Extensions read as C; headers are treated as C unless their name says C++.
pub const C_EXTENSIONS: &[&str] = &["c", "h"];
pub const CPP_EXTENSIONS: &[&str] = &[
    "cc", "cpp", "cxx", "c++", "hh", "hpp", "hxx", "h++", "inl", "ipp", "tpp",
];

#[derive(Clone, Copy, Debug, Default)]
pub struct NativeImportAdapter;

impl LanguageImportAdapter for NativeImportAdapter {
    fn id(&self) -> &'static str {
        "c-cpp-includes"
    }

    fn version(&self) -> u32 {
        ADAPTER_VERSION
    }

    fn profile_for(&self, path: &RootRelativePath) -> Option<LanguageProfile> {
        let extension = path.as_path().extension()?.to_str()?;
        if C_EXTENSIONS.contains(&extension) {
            Some(LanguageProfile::C)
        } else if CPP_EXTENSIONS.contains(&extension) {
            Some(LanguageProfile::Cpp)
        } else {
            None
        }
    }

    fn forms(&self) -> Vec<ImportForm> {
        vec![ImportForm::Include]
    }

    fn extract(
        &self,
        path: &RootRelativePath,
        profile: LanguageProfile,
        source: &str,
        limits: &ImportLimits,
    ) -> ExtractedFile {
        let mut out = ExtractedFile::default();
        let mut parser = tree_sitter::Parser::new();
        // The C++ grammar reads C as well; includes are the same preprocessor directive in both.
        if parser
            .set_language(&tree_sitter_cpp::LANGUAGE.into())
            .is_err()
        {
            out.diagnostics.push("grammar is unavailable".into());
            return out;
        }
        let Some(tree) = parser.parse(source, None) else {
            out.diagnostics.push("parse produced no tree".into());
            return out;
        };
        let builder = FactBuilder { path, profile };
        let mut stack = vec![tree.root_node()];
        while let Some(node) = stack.pop() {
            if out.facts.len() >= limits.max_facts_per_file {
                out.truncated = true;
                break;
            }
            if node.kind() == "preproc_include" {
                include(node, source, &builder, &mut out);
                continue;
            }
            let mut cursor = node.walk();
            let children: Vec<Node<'_>> = node.children(&mut cursor).collect();
            stack.extend(children.into_iter().rev());
        }
        out.facts.sort_by_key(|f| f.span.start);
        out
    }
}

fn include(node: Node<'_>, source: &str, builder: &FactBuilder<'_>, out: &mut ExtractedFile) {
    let Some(path_node) = node.child_by_field_name("path") else {
        return;
    };
    let raw = &source[path_node.start_byte()..path_node.end_byte()];
    let span = ByteSpan {
        start: node.start_byte(),
        end: node.end_byte(),
    };
    match path_node.kind() {
        "string_literal" | "system_lib_string" => {
            let system = path_node.kind() == "system_lib_string";
            let spelling = raw.trim_matches(['"', '<', '>']).to_owned();
            let mut fact = builder.fact(span, "", spelling, ImportForm::Include);
            if system {
                fact.scope = vec!["system".into()];
            }
            out.facts.push(fact);
        }
        _ => {
            // `#include MACRO`: the header is not known without running the preprocessor.
            let mut fact = builder.fact(span, "", raw.to_owned(), ImportForm::Include);
            fact.staticness = Staticness::Dynamic;
            fact.literal_specifier = false;
            out.facts.push(fact);
        }
    }
}
