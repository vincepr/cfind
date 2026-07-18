use std::path::Path;

use tree_sitter::{Language, Node};

use super::{Definition, LanguageAdapter};

pub(super) struct JavaScriptAdapter;

impl LanguageAdapter for JavaScriptAdapter {
    fn name(&self) -> &'static str {
        "javascript"
    }

    fn aliases(&self) -> &'static [&'static str] {
        &["javascript", "js"]
    }

    fn extensions(&self) -> &'static [&'static str] {
        &["js", "jsx", "mjs", "cjs"]
    }

    fn grammar(&self, _path: Option<&Path>) -> Language {
        tree_sitter_javascript::LANGUAGE.into()
    }

    fn definition<'tree>(&self, node: Node<'tree>) -> Option<Definition<'tree>> {
        let kind = match node.kind() {
            "class_declaration" => "class",
            "function_declaration" | "generator_function_declaration" => "function",
            "method_definition" => "method",
            "variable_declarator" => {
                let value = node.child_by_field_name("value")?;
                if !matches!(value.kind(), "arrow_function" | "function_expression") {
                    return None;
                }
                "function"
            }
            _ => return None,
        };
        Definition::named(node, kind)
    }
}

#[cfg(test)]
mod tests {
    use crate::{config::SupportedLanguage, language::parse_source};

    #[test]
    fn extracts_functions_without_indexing_plain_variables() {
        let source = br#"
            class DatabaseContext { connect() {} }
            const loadDatabase = () => 1;
            const timeout = 30;
        "#;
        let symbols = parse_source(source, SupportedLanguage::JavaScript).unwrap();
        assert!(
            symbols
                .iter()
                .any(|symbol| symbol.name == "DatabaseContext")
        );
        assert!(symbols.iter().any(|symbol| symbol.name == "loadDatabase"));
        assert!(symbols.iter().all(|symbol| symbol.name != "timeout"));
    }
}
