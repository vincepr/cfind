use std::{fs, path::Path};

use anyhow::{Context, Result};
use tree_sitter::{Language, Node, Parser};

use crate::{Symbol, config::SupportedLanguage};

pub fn parse_file(path: &Path, language: SupportedLanguage) -> Result<Vec<Symbol>> {
    let source = fs::read(path).with_context(|| format!("could not read {}", path.display()))?;
    let tsx = path.extension().is_some_and(|extension| extension == "tsx");
    parse_source_with_dialect(&source, language, tsx).with_context(|| {
        format!(
            "could not parse {} as {}",
            path.display(),
            language.as_str()
        )
    })
}

pub fn parse_source(source: &[u8], language: SupportedLanguage) -> Result<Vec<Symbol>> {
    parse_source_with_dialect(source, language, false)
}

fn parse_source_with_dialect(
    source: &[u8],
    language: SupportedLanguage,
    tsx: bool,
) -> Result<Vec<Symbol>> {
    let grammar: Language = match language {
        SupportedLanguage::Rust => tree_sitter_rust::LANGUAGE.into(),
        SupportedLanguage::JavaScript => tree_sitter_javascript::LANGUAGE.into(),
        SupportedLanguage::TypeScript if tsx => tree_sitter_typescript::LANGUAGE_TSX.into(),
        SupportedLanguage::TypeScript => tree_sitter_typescript::LANGUAGE_TYPESCRIPT.into(),
        SupportedLanguage::CSharp => tree_sitter_c_sharp::LANGUAGE.into(),
    };
    let mut parser = Parser::new();
    parser.set_language(&grammar)?;
    let tree = parser
        .parse(source, None)
        .context("Tree-sitter did not produce a syntax tree")?;
    let mut symbols = Vec::new();
    collect_symbols(tree.root_node(), source, language, None, &mut symbols);
    Ok(symbols)
}

fn collect_symbols(
    node: Node<'_>,
    source: &[u8],
    language: SupportedLanguage,
    parent: Option<&str>,
    symbols: &mut Vec<Symbol>,
) {
    let definition = symbol_kind(node, language).and_then(|kind| {
        let name_node = symbol_name_node(node, language)?;
        let name = name_node.utf8_text(source).ok()?.trim().to_owned();
        if name.is_empty() {
            return None;
        }
        Some((kind, name))
    });

    let next_parent = if let Some((kind, name)) = definition {
        let start = node.start_position();
        let end = node.end_position();
        symbols.push(Symbol {
            name: name.clone(),
            kind: kind.to_owned(),
            start_line: start.row + 1,
            start_column: start.column + 1,
            end_line: end.row + 1,
            end_column: end.column + 1,
            parent: parent.map(str::to_owned),
        });
        Some(name)
    } else {
        parent.map(str::to_owned)
    };

    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        collect_symbols(child, source, language, next_parent.as_deref(), symbols);
    }
}

fn symbol_name_node<'tree>(node: Node<'tree>, language: SupportedLanguage) -> Option<Node<'tree>> {
    let name = node.child_by_field_name("name")?;
    if (language == SupportedLanguage::JavaScript || language == SupportedLanguage::TypeScript)
        && node.kind() == "variable_declarator"
    {
        let value = node.child_by_field_name("value")?;
        if !matches!(value.kind(), "arrow_function" | "function_expression") {
            return None;
        }
    }
    Some(name)
}

fn symbol_kind(node: Node<'_>, language: SupportedLanguage) -> Option<&'static str> {
    let kind = match language {
        SupportedLanguage::Rust => match node.kind() {
            "struct_item" => "struct",
            "enum_item" => "enum",
            "union_item" => "union",
            "trait_item" => "trait",
            "type_item" => "type",
            "function_item" | "function_signature_item" => "function",
            "const_item" => "constant",
            "static_item" => "static",
            _ => return None,
        },
        SupportedLanguage::JavaScript => match node.kind() {
            "class_declaration" => "class",
            "function_declaration" | "generator_function_declaration" => "function",
            "method_definition" => "method",
            "variable_declarator" => "function",
            _ => return None,
        },
        SupportedLanguage::TypeScript => match node.kind() {
            "class_declaration" | "abstract_class_declaration" => "class",
            "interface_declaration" => "interface",
            "type_alias_declaration" => "type",
            "enum_declaration" => "enum",
            "function_declaration" | "generator_function_declaration" => "function",
            "method_definition" | "method_signature" | "abstract_method_signature" => "method",
            "variable_declarator" => "function",
            _ => return None,
        },
        SupportedLanguage::CSharp => match node.kind() {
            "class_declaration" => "class",
            "record_declaration" => "record",
            "struct_declaration" => "struct",
            "interface_declaration" => "interface",
            "enum_declaration" => "enum",
            "delegate_declaration" => "delegate",
            "method_declaration" => "method",
            "constructor_declaration" => "constructor",
            "property_declaration" => "property",
            _ => return None,
        },
    };
    Some(kind)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extracts_rust_types_and_methods() {
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

    #[test]
    fn extracts_csharp_records_and_methods() {
        let source = br#"
            public record DatabaseEntity(int Id);
            public class DatabaseContext {
                public void Save() {}
            }
        "#;
        let symbols = parse_source(source, SupportedLanguage::CSharp).unwrap();
        assert!(
            symbols
                .iter()
                .any(|symbol| symbol.name == "DatabaseEntity" && symbol.kind == "record")
        );
        assert!(
            symbols
                .iter()
                .any(|symbol| symbol.name == "Save" && symbol.kind == "method")
        );
    }

    #[test]
    fn extracts_typescript_interfaces_and_arrow_functions() {
        let source = br#"
            interface DatabaseEntity { id: number }
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
    }
}
