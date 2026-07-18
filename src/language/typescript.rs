use std::path::Path;

use tree_sitter::{Language, Node};

use super::{Definition, LanguageAdapter};

pub(super) struct TypeScriptAdapter;

impl LanguageAdapter for TypeScriptAdapter {
    fn name(&self) -> &'static str {
        "typescript"
    }

    fn aliases(&self) -> &'static [&'static str] {
        &["typescript", "ts"]
    }

    fn extensions(&self) -> &'static [&'static str] {
        &["ts", "tsx", "mts", "cts"]
    }

    fn grammar(&self, path: Option<&Path>) -> Language {
        if path.is_some_and(|path| path.extension().is_some_and(|extension| extension == "tsx")) {
            tree_sitter_typescript::LANGUAGE_TSX.into()
        } else {
            tree_sitter_typescript::LANGUAGE_TYPESCRIPT.into()
        }
    }

    fn definition<'tree>(&self, node: Node<'tree>) -> Option<Definition<'tree>> {
        let kind = match node.kind() {
            "class_declaration" | "abstract_class_declaration" => "class",
            "interface_declaration" => "interface",
            "type_alias_declaration" => "type",
            "enum_declaration" => "enum",
            "function_declaration" | "generator_function_declaration" | "function_signature" => {
                "function"
            }
            "method_definition" | "method_signature" | "abstract_method_signature" => "method",
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
    fn extracts_interfaces_and_arrow_functions() {
        let source = br#"
            interface DatabaseEntity { id: number }
            declare function createDatabase(): DatabaseEntity;
            const loadDatabase = () => 1;
        "#;
        let symbols = parse_source(source, SupportedLanguage::TypeScript).unwrap();
        assert!(
            symbols
                .iter()
                .any(|symbol| symbol.name == "DatabaseEntity" && symbol.kind == "interface")
        );
        assert!(
            symbols
                .iter()
                .any(|symbol| symbol.name == "loadDatabase" && symbol.kind == "function")
        );
        assert!(
            symbols
                .iter()
                .any(|symbol| symbol.name == "createDatabase" && symbol.kind == "function")
        );
    }
}
