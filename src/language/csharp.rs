use std::path::Path;

use tree_sitter::{Language, Node};

use super::{Definition, LanguageAdapter, qualify_namespace};

pub(super) struct CSharpAdapter;

impl LanguageAdapter for CSharpAdapter {
    fn name(&self) -> &'static str {
        "csharp"
    }

    fn aliases(&self) -> &'static [&'static str] {
        &["csharp", "c#", "cs"]
    }

    fn extensions(&self) -> &'static [&'static str] {
        &["cs"]
    }

    fn grammar(&self, _path: Option<&Path>) -> Language {
        tree_sitter_c_sharp::LANGUAGE.into()
    }

    fn definition<'tree>(&self, node: Node<'tree>) -> Option<Definition<'tree>> {
        let kind = match node.kind() {
            "namespace_declaration" | "file_scoped_namespace_declaration" => "namespace",
            "class_declaration" | "record_declaration" | "struct_declaration" => "class",
            "interface_declaration" => "interface",
            "enum_declaration" => "enum",
            "delegate_declaration" => "delegate",
            "method_declaration" => "method",
            "constructor_declaration" => "constructor",
            "property_declaration" => "property",
            _ => return None,
        };
        Definition::named(node, kind)
    }

    fn namespace_after(
        &self,
        node: Node<'_>,
        source: &[u8],
        current: Option<&str>,
    ) -> Option<String> {
        if node.kind() != "file_scoped_namespace_declaration" {
            return None;
        }
        let name = node
            .child_by_field_name("name")?
            .utf8_text(source)
            .ok()?
            .trim();
        (!name.is_empty()).then(|| qualify_namespace(current, name))
    }
}

#[cfg(test)]
mod tests {
    use crate::{config::SupportedLanguage, language::parse_source};

    #[test]
    fn normalizes_classes_records_and_structs_to_class() {
        let source = br#"
            namespace Acme.Data {
                public record DatabaseEntity(int Id);
                public struct DatabaseValue {}
                public class DatabaseContext {
                    public void Save() {}
                }
            }
        "#;
        let symbols = parse_source(source, SupportedLanguage::CSharp).unwrap();
        assert!(
            symbols
                .iter()
                .any(|symbol| symbol.name == "DatabaseEntity" && symbol.kind == "class")
        );
        assert!(
            symbols
                .iter()
                .any(|symbol| symbol.name == "DatabaseValue" && symbol.kind == "class")
        );
        assert!(
            symbols
                .iter()
                .all(|symbol| !matches!(symbol.kind.as_str(), "record" | "struct"))
        );
        assert!(
            symbols
                .iter()
                .any(|symbol| symbol.name == "Save" && symbol.kind == "method")
        );
        assert!(
            symbols
                .iter()
                .any(|symbol| { symbol.name == "Acme.Data" && symbol.kind == "namespace" })
        );
        assert!(symbols.iter().any(|symbol| {
            symbol.name == "DatabaseContext"
                && symbol.namespace.as_deref() == Some("Acme.Data")
                && symbol.qualified_name == "Acme.Data.DatabaseContext"
        }));
    }

    #[test]
    fn applies_file_scoped_namespaces_to_following_symbols() {
        let source = b"namespace Acme.Data;\npublic class DatabaseContext {}\n";
        let symbols = parse_source(source, SupportedLanguage::CSharp).unwrap();
        assert!(symbols.iter().any(|symbol| {
            symbol.name == "DatabaseContext"
                && symbol.namespace.as_deref() == Some("Acme.Data")
                && symbol.qualified_name == "Acme.Data.DatabaseContext"
        }));
    }

    #[test]
    fn qualifies_nested_definitions_with_the_full_enclosing_chain() {
        let source = br#"
            namespace Acme.Tools {
                public class Container {
                    public class PaymentProcessor {
                        public void Run() {}
                    }
                }
            }
        "#;
        let symbols = parse_source(source, SupportedLanguage::CSharp).unwrap();
        let run = symbols.iter().find(|symbol| symbol.name == "Run").unwrap();
        assert_eq!(run.parent.as_deref(), Some("PaymentProcessor"));
        assert_eq!(
            run.qualified_name,
            "Acme.Tools.Container.PaymentProcessor.Run"
        );
    }
}
