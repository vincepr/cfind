use std::path::Path;

use tree_sitter::{Language, Node};

use super::{Definition, LanguageAdapter};

pub(super) struct RustAdapter;

impl LanguageAdapter for RustAdapter {
    fn name(&self) -> &'static str {
        "rust"
    }

    fn aliases(&self) -> &'static [&'static str] {
        &["rust", "rs"]
    }

    fn extensions(&self) -> &'static [&'static str] {
        &["rs"]
    }

    fn grammar(&self, _path: Option<&Path>) -> Language {
        tree_sitter_rust::LANGUAGE.into()
    }

    fn definition<'tree>(&self, node: Node<'tree>) -> Option<Definition<'tree>> {
        let kind = match node.kind() {
            "struct_item" => "struct",
            "enum_item" => "enum",
            "union_item" => "union",
            "trait_item" => "trait",
            "type_item" => "type",
            "function_item" | "function_signature_item" => "function",
            "const_item" => "constant",
            "static_item" => "static",
            _ => return None,
        };
        Definition::named(node, kind)
    }
}

#[cfg(test)]
mod tests {
    use crate::{config::SupportedLanguage, language::parse_source};

    #[test]
    fn extracts_types_and_methods() {
        let source = br#"
            struct DatabaseContext;
            impl DatabaseContext {
                fn connect(&self) {}
            }
        "#;
        let symbols = parse_source(source, SupportedLanguage::Rust).unwrap();
        assert!(
            symbols
                .iter()
                .any(|symbol| symbol.name == "DatabaseContext" && symbol.kind == "struct")
        );
        assert!(
            symbols
                .iter()
                .any(|symbol| symbol.name == "connect" && symbol.kind == "function")
        );
    }
}
