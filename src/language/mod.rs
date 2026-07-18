mod csharp;
mod javascript;
mod rust;
mod typescript;

use std::{fs, path::Path};

use anyhow::{Context, Result};
use tree_sitter::{Language, Node, Parser};

use crate::{Symbol, config::SupportedLanguage};

use self::{
    csharp::CSharpAdapter, javascript::JavaScriptAdapter, rust::RustAdapter,
    typescript::TypeScriptAdapter,
};

pub(super) struct Definition<'tree> {
    pub kind: &'static str,
    pub name: Node<'tree>,
}

impl<'tree> Definition<'tree> {
    pub fn named(node: Node<'tree>, kind: &'static str) -> Option<Self> {
        Some(Self {
            kind,
            name: node.child_by_field_name("name")?,
        })
    }
}

pub(super) trait LanguageAdapter {
    fn name(&self) -> &'static str;
    fn aliases(&self) -> &'static [&'static str];
    fn extensions(&self) -> &'static [&'static str];
    fn grammar(&self, path: Option<&Path>) -> Language;
    fn definition<'tree>(&self, node: Node<'tree>) -> Option<Definition<'tree>>;

    fn namespace_after(
        &self,
        _node: Node<'_>,
        _source: &[u8],
        _current: Option<&str>,
    ) -> Option<String> {
        None
    }
}

static RUST: RustAdapter = RustAdapter;
static JAVASCRIPT: JavaScriptAdapter = JavaScriptAdapter;
static TYPESCRIPT: TypeScriptAdapter = TypeScriptAdapter;
static CSHARP: CSharpAdapter = CSharpAdapter;

fn adapter(language: SupportedLanguage) -> &'static dyn LanguageAdapter {
    match language {
        SupportedLanguage::Rust => &RUST,
        SupportedLanguage::JavaScript => &JAVASCRIPT,
        SupportedLanguage::TypeScript => &TYPESCRIPT,
        SupportedLanguage::CSharp => &CSHARP,
    }
}

pub(crate) fn language_from_alias(value: &str) -> Option<SupportedLanguage> {
    let value = value.trim().to_ascii_lowercase();
    SupportedLanguage::ALL
        .into_iter()
        .find(|language| adapter(*language).aliases().contains(&value.as_str()))
}

pub(crate) fn language_from_path(path: &Path) -> Option<SupportedLanguage> {
    language_from_extension(path.extension()?.to_str()?)
}

pub(crate) fn language_from_extension(extension: &str) -> Option<SupportedLanguage> {
    let extension = extension.to_ascii_lowercase();
    SupportedLanguage::ALL.into_iter().find(|language| {
        adapter(*language)
            .extensions()
            .contains(&extension.as_str())
    })
}

pub(crate) fn language_name(language: SupportedLanguage) -> &'static str {
    adapter(language).name()
}

pub fn parse_file(path: &Path, language: SupportedLanguage) -> Result<Vec<Symbol>> {
    let source = fs::read(path).with_context(|| format!("could not read {}", path.display()))?;
    parse_source_with_adapter(&source, language, Some(path)).with_context(|| {
        format!(
            "could not parse {} as {}",
            path.display(),
            language.as_str()
        )
    })
}

pub fn parse_source(source: &[u8], language: SupportedLanguage) -> Result<Vec<Symbol>> {
    parse_source_with_adapter(source, language, None)
}

fn parse_source_with_adapter(
    source: &[u8],
    language: SupportedLanguage,
    path: Option<&Path>,
) -> Result<Vec<Symbol>> {
    let adapter = adapter(language);
    let mut parser = Parser::new();
    parser.set_language(&adapter.grammar(path))?;
    let tree = parser
        .parse(source, None)
        .context("Tree-sitter did not produce a syntax tree")?;
    let mut symbols = Vec::new();
    collect_symbols(tree.root_node(), source, adapter, None, None, &mut symbols);
    Ok(symbols)
}

fn collect_symbols(
    node: Node<'_>,
    source: &[u8],
    adapter: &dyn LanguageAdapter,
    parent: Option<&str>,
    namespace: Option<&str>,
    symbols: &mut Vec<Symbol>,
) {
    let definition = adapter.definition(node).and_then(|definition| {
        let name = definition.name.utf8_text(source).ok()?.trim().to_owned();
        (!name.is_empty()).then_some((definition.kind, name))
    });

    let mut next_parent = parent.map(str::to_owned);
    let mut next_namespace = namespace.map(str::to_owned);
    if let Some((kind, name)) = definition {
        let start = node.start_position();
        let end = node.end_position();
        let is_namespace = kind == "namespace";
        let indexed_name = if is_namespace {
            qualify_namespace(namespace, &name)
        } else {
            name.clone()
        };
        symbols.push(Symbol {
            name: indexed_name.clone(),
            kind: kind.to_owned(),
            namespace: if is_namespace {
                None
            } else {
                namespace.map(str::to_owned)
            },
            start_line: start.row + 1,
            start_column: start.column + 1,
            end_line: end.row + 1,
            end_column: end.column + 1,
            parent: if is_namespace {
                None
            } else {
                parent.map(str::to_owned)
            },
        });
        if is_namespace {
            next_namespace = Some(indexed_name);
        } else {
            next_parent = Some(name);
        }
    }

    let mut cursor = node.walk();
    let mut sibling_namespace = next_namespace.clone();
    for child in node.children(&mut cursor) {
        collect_symbols(
            child,
            source,
            adapter,
            next_parent.as_deref(),
            sibling_namespace.as_deref(),
            symbols,
        );
        if let Some(namespace) = adapter.namespace_after(child, source, next_namespace.as_deref()) {
            sibling_namespace = Some(namespace);
        }
    }
}

pub(super) fn qualify_namespace(parent: Option<&str>, name: &str) -> String {
    match parent {
        Some(parent) if name == parent || name.starts_with(&format!("{parent}.")) => {
            name.to_owned()
        }
        Some(parent) => format!("{parent}.{name}"),
        None => name.to_owned(),
    }
}
