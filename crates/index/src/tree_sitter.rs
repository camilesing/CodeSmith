//! Built-in tree-sitter symbol backend (feature `tree-sitter`).
//!
//! Extraction is table-driven: per language, a list of node kinds mapping
//! to [`SymbolKind`]s, with the symbol name taken from the node's `name`
//! field. Container scoping (methods inside impls/classes/traits) is
//! handled with a small stack while walking. References are **lexical**:
//! identifier leaves whose text equals a known symbol name, excluding the
//! definition sites themselves.
//!
//! JS/TS coverage is deliberately declaration-form only
//! (`function_declaration`, `method_definition`, …). Definitions whose
//! symbol-ness depends on the value shape — arrow/function-expression
//! consts (`const f = () => {}`), anonymous `export default function`,
//! object-literal methods, TS `method_signature` members — yield no
//! symbol: the flat kind-mapping table here has no way to key on a
//! declarator's value, so extending coverage needs structural rules, not
//! new table rows.

use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::path::Path;

use anyhow::{Context, Result};
use std::sync::Arc;

use crate::backend::{
    Extraction, IndexBackend, IndexBackendConfig, IndexBackendFactory, IndexCapability,
};
use crate::types::{Language, Location, Occurrence, OccurrenceRole, Symbol, SymbolKind};

use tree_sitter::{Node, Parser, Tree};

/// Factory for the tree-sitter symbol backend.
pub struct TreeSitterFactory;

impl IndexBackendFactory for TreeSitterFactory {
    fn id(&self) -> &str {
        "tree-sitter"
    }

    fn capabilities(&self) -> &'static [IndexCapability] {
        &[IndexCapability::Symbols]
    }

    fn build(&self, cfg: &IndexBackendConfig) -> Result<Arc<dyn IndexBackend>> {
        Ok(Arc::new(TreeSitterBackend {
            languages: cfg.languages.clone(),
        }))
    }
}

/// The backend itself: pure function of (source, language) — no IO.
pub struct TreeSitterBackend {
    languages: Vec<Language>,
}

impl IndexBackend for TreeSitterBackend {
    fn id(&self) -> &str {
        "tree-sitter"
    }

    fn supported_languages(&self) -> &[Language] {
        &self.languages
    }

    fn extract(&self, file: &Path, source: &str, lang: Language) -> Result<Extraction> {
        if !self.languages.contains(&lang) {
            anyhow::bail!("language {lang:?} not enabled for this backend");
        }
        extract_symbols(file, source, lang)
    }
}

/// One extraction rule: a tree-sitter node kind producing a symbol.
struct Rule {
    node: &'static str,
    kind: SymbolKind,
    /// Function-like nodes become [`SymbolKind::Method`] (with container)
    /// when walked inside an impl/trait/class scope.
    method_like: bool,
}

const fn rule(node: &'static str, kind: SymbolKind) -> Rule {
    Rule {
        node,
        kind,
        method_like: false,
    }
}

const fn rule_fn_like(node: &'static str) -> Rule {
    Rule {
        node,
        kind: SymbolKind::Function,
        method_like: true,
    }
}

const RUST_RULES: &[Rule] = &[
    rule_fn_like("function_item"),
    rule_fn_like("function_signature_item"),
    rule("struct_item", SymbolKind::Struct),
    rule("union_item", SymbolKind::Struct),
    rule("enum_item", SymbolKind::Enum),
    rule("trait_item", SymbolKind::Trait),
    rule("type_item", SymbolKind::TypeAlias),
    rule("const_item", SymbolKind::Constant),
    rule("static_item", SymbolKind::Constant),
    rule("mod_item", SymbolKind::Module),
    rule("macro_definition", SymbolKind::Macro),
];

const PYTHON_RULES: &[Rule] = &[
    rule_fn_like("function_definition"),
    rule("class_definition", SymbolKind::Class),
];

const JAVASCRIPT_RULES: &[Rule] = &[
    rule("function_declaration", SymbolKind::Function),
    rule("generator_function_declaration", SymbolKind::Function),
    rule("class_declaration", SymbolKind::Class),
    rule("method_definition", SymbolKind::Method),
];

const TYPESCRIPT_RULES: &[Rule] = &[
    rule("function_declaration", SymbolKind::Function),
    rule("generator_function_declaration", SymbolKind::Function),
    rule("class_declaration", SymbolKind::Class),
    rule("method_definition", SymbolKind::Method),
    rule("interface_declaration", SymbolKind::Interface),
    rule("type_alias_declaration", SymbolKind::TypeAlias),
    rule("enum_declaration", SymbolKind::Enum),
];

fn rules_for(lang: Language) -> &'static [Rule] {
    match lang {
        Language::Rust => RUST_RULES,
        Language::Python => PYTHON_RULES,
        Language::JavaScript => JAVASCRIPT_RULES,
        Language::TypeScript => TYPESCRIPT_RULES,
        // Go handled structurally (type_spec/method_declaration carry the
        // interesting shape in their children, not their kind).
        Language::Go => &[],
    }
}

/// Identifier leaf kinds eligible as lexical references.
fn ident_kinds_for(lang: Language) -> &'static [&'static str] {
    match lang {
        Language::Rust => &["identifier", "type_identifier"],
        Language::Python => &["identifier"],
        Language::JavaScript | Language::TypeScript => &["identifier", "type_identifier"],
        // field_identifier covers Go selector expressions (`x.Method`).
        Language::Go => &["identifier", "type_identifier", "field_identifier"],
    }
}

/// A container scope on the walk stack: either an impl/trait/class the
/// walked node is a direct member of, or a function body whose nested
/// definitions are plain functions, not methods.
enum Scope {
    Container(String),
    InFunction,
}

struct Ctx<'a> {
    source: &'a str,
    rel_path: String,
    lang: Language,
    symbols: Vec<Symbol>,
    occurrences: Vec<Occurrence>,
    def_name_node_ids: HashSet<usize>,
    def_names: HashSet<String>,
    container: Vec<Scope>,
    line_starts: Vec<usize>,
}

impl<'a> Ctx<'a> {
    fn text_of(&self, node: Node<'_>) -> Option<&'a str> {
        self.source.get(node.byte_range())
    }

    fn line_at(&self, row: usize) -> Option<String> {
        let start = *self.line_starts.get(row)?;
        let end = self
            .line_starts
            .get(row + 1)
            .copied()
            .unwrap_or(self.source.len());
        let line = self.source[start..end].trim();
        let truncated: String = line.chars().take(120).collect();
        Some(truncated)
    }

    fn push_symbol(
        &mut self,
        name: &str,
        kind: SymbolKind,
        container: Option<String>,
        node: Node<'_>,
        name_node: Node<'_>,
    ) {
        let location = location_of(node);
        self.symbols.push(Symbol {
            name: name.to_string(),
            kind,
            container,
            path: self.rel_path.clone(),
            location,
            signature: self.line_at(node.start_position().row),
        });
        self.def_name_node_ids.insert(name_node.id());
        self.def_names.insert(name.to_string());
        self.occurrences.push(Occurrence {
            name: name.to_string(),
            role: OccurrenceRole::Definition,
            path: self.rel_path.clone(),
            line: location.line,
        });
    }
}

/// Extract symbols + lexical occurrences from `source`.
pub fn extract_symbols(file: &Path, source: &str, lang: Language) -> Result<Extraction> {
    let tree = parse_source(file, source, lang)?;
    let rel_path = file.to_string_lossy().replace('\\', "/");
    let line_starts: Vec<usize> = std::iter::once(0)
        .chain(source.match_indices('\n').map(|(i, _)| i + 1))
        .collect();
    let mut ctx = Ctx {
        source,
        rel_path,
        lang,
        symbols: Vec::new(),
        occurrences: Vec::new(),
        def_name_node_ids: HashSet::new(),
        def_names: HashSet::new(),
        container: Vec::new(),
        line_starts,
    };
    visit(tree.root_node(), &mut ctx);
    collect_references(tree.root_node(), &mut ctx);
    Ok(Extraction {
        symbols: ctx.symbols,
        occurrences: ctx.occurrences,
    })
}

fn visit(node: Node<'_>, ctx: &mut Ctx<'_>) {
    let kind = node.kind();
    let mut pushed_function = false;

    match ctx.lang {
        Language::Go => {
            if kind == "type_spec" {
                visit_go_type_spec(node, ctx);
            } else if kind == "type_alias" {
                // `type Handler = int` parses as type_alias, not type_spec.
                if let Some(name_node) = node.child_by_field_name("name")
                    && let Some(name) = ctx.text_of(name_node)
                {
                    ctx.push_symbol(name, SymbolKind::TypeAlias, None, node, name_node);
                }
            } else if kind == "method_declaration" {
                visit_go_method(node, ctx);
            } else if kind == "function_declaration"
                && let Some(name_node) = node.child_by_field_name("name")
                && let Some(name) = ctx.text_of(name_node)
            {
                ctx.push_symbol(name, SymbolKind::Function, None, node, name_node);
            }
        }
        _ => {
            if let Some(rule) = rules_for(ctx.lang).iter().find(|r| r.node == kind) {
                visit_rule_symbol(rule, node, ctx);
                pushed_function = rule.method_like;
            }
        }
    }

    let pushed = push_container_scope(node, kind, ctx);
    if pushed_function {
        ctx.container.push(Scope::InFunction);
    }
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        visit(child, ctx);
    }
    if pushed {
        ctx.container.pop();
    }
    if pushed_function {
        ctx.container.pop();
    }
}

/// Standard rule-driven symbol: name from the node's `name` field.
fn visit_rule_symbol(rule: &Rule, node: Node<'_>, ctx: &mut Ctx<'_>) {
    let Some(name_node) = node.child_by_field_name("name") else {
        return;
    };
    let Some(name) = ctx.text_of(name_node) else {
        return;
    };
    // A function-like is a method only when directly inside an
    // impl/trait/class scope; one nested in another function body (a
    // `fn` inside a method, a `def` inside a method) is a plain function.
    let (kind, container) = if rule.method_like {
        match ctx.container.last() {
            Some(Scope::Container(name)) => (SymbolKind::Method, Some(name.clone())),
            _ => (rule.kind, None),
        }
    } else if rule.kind == SymbolKind::Method {
        let container = ctx.container.iter().rev().find_map(|scope| match scope {
            Scope::Container(name) => Some(name.clone()),
            Scope::InFunction => None,
        });
        (SymbolKind::Method, container)
    } else {
        (rule.kind, None)
    };
    ctx.push_symbol(name, kind, container, node, name_node);
}

fn visit_go_type_spec(node: Node<'_>, ctx: &mut Ctx<'_>) {
    let Some(name_node) = node.child_by_field_name("name") else {
        return;
    };
    let Some(name) = ctx.text_of(name_node) else {
        return;
    };
    let kind = match node.child_by_field_name("type").map(|t| t.kind()) {
        Some("struct_type") => SymbolKind::Struct,
        Some("interface_type") => SymbolKind::Interface,
        _ => SymbolKind::TypeAlias,
    };
    ctx.push_symbol(name, kind, None, node, name_node);
}

fn visit_go_method(node: Node<'_>, ctx: &mut Ctx<'_>) {
    let Some(name_node) = node.child_by_field_name("name") else {
        return;
    };
    let Some(name) = ctx.text_of(name_node) else {
        return;
    };
    let container = node
        .child_by_field_name("receiver")
        .and_then(|r| ctx.text_of(r))
        .map(|recv| {
            let inner = recv.trim().trim_start_matches('(').trim_end_matches(')');
            inner
                .split_whitespace()
                .next_back()
                .unwrap_or(inner)
                .trim_start_matches('*')
                // `*Registry[T]` → `Registry`: container joins by base type
                // name, so generics-bearing receivers must not leak the
                // parameter list.
                .split('<')
                .next()
                .unwrap_or("")
                .trim()
                .to_string()
        });
    ctx.push_symbol(name, SymbolKind::Method, container, node, name_node);
}

/// Push a container scope (impl/trait/class) so nested function-likes
/// become methods attributed to it.
fn push_container_scope(node: Node<'_>, kind: &str, ctx: &mut Ctx<'_>) -> bool {
    let name = match ctx.lang {
        Language::Rust => {
            if kind == "impl_item" {
                node.child_by_field_name("type")
            } else if kind == "trait_item" {
                node.child_by_field_name("name")
            } else {
                return false;
            }
        }
        Language::Python => {
            if kind == "class_definition" {
                node.child_by_field_name("name")
            } else {
                return false;
            }
        }
        Language::JavaScript | Language::TypeScript => {
            if kind == "class_declaration" {
                node.child_by_field_name("name")
            } else {
                return false;
            }
        }
        Language::Go => return false,
    };
    if let Some(name_node) = name
        && let Some(text) = ctx.text_of(name_node)
    {
        // Strip generic parameters (`impl<T> Registry<T>` → `Registry`, and
        // `path::To::Type<…>` keeps its path): downstream container joins
        // use the base type name, so generics must not leak into it.
        let base = text.split('<').next().unwrap_or("");
        ctx.container
            .push(Scope::Container(base.trim().to_string()));
        return true;
    }
    false
}

fn collect_references(node: Node<'_>, ctx: &mut Ctx<'_>) {
    let idents = ident_kinds_for(ctx.lang);
    if idents.contains(&node.kind())
        && node.child_count() == 0
        && let Some(name) = ctx.text_of(node)
        && ctx.def_names.contains(name)
        && !ctx.def_name_node_ids.contains(&node.id())
        // A Go struct-field name (`field_identifier` under a
        // `field_declaration`) is a declaration site, not a reference to a
        // same-named indexed symbol.
        && !(ctx.lang == Language::Go
            && node.kind() == "field_identifier"
            && node
                .parent()
                .is_some_and(|p| p.kind() == "field_declaration"))
    {
        ctx.occurrences.push(Occurrence {
            name: name.to_string(),
            role: OccurrenceRole::Reference,
            path: ctx.rel_path.clone(),
            line: node.start_position().row as u32 + 1,
        });
    }
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        collect_references(child, ctx);
    }
}

fn location_of(node: Node<'_>) -> Location {
    let start = node.start_position();
    let end = node.end_position();
    Location {
        line: start.row as u32 + 1,
        col: start.column as u32 + 1,
        end_line: end.row as u32 + 1,
        end_col: end.column as u32 + 1,
    }
}

thread_local! {
    /// One parser per language per thread (`Parser` is not `Sync`),
    /// reset between files instead of rebuilt per extraction.
    static PARSERS: RefCell<HashMap<tree_sitter::Language, Parser>> =
        RefCell::new(HashMap::new());
}

fn grammar_for(file: &Path, lang: Language) -> tree_sitter::Language {
    match lang {
        Language::Rust => tree_sitter_rust::LANGUAGE.into(),
        Language::Python => tree_sitter_python::LANGUAGE.into(),
        Language::JavaScript => tree_sitter_javascript::LANGUAGE.into(),
        Language::TypeScript => {
            let is_tsx = file
                .extension()
                .and_then(|e| e.to_str())
                .is_some_and(|e| e.eq_ignore_ascii_case("tsx"));
            if is_tsx {
                tree_sitter_typescript::LANGUAGE_TSX.into()
            } else {
                tree_sitter_typescript::LANGUAGE_TYPESCRIPT.into()
            }
        }
        Language::Go => tree_sitter_go::LANGUAGE.into(),
    }
}

fn parse_source(file: &Path, source: &str, lang: Language) -> Result<Tree> {
    let language = grammar_for(file, lang);
    PARSERS.with(|cache| {
        let mut cache = cache.borrow_mut();
        let parser = match cache.entry(language) {
            std::collections::hash_map::Entry::Occupied(entry) => entry.into_mut(),
            std::collections::hash_map::Entry::Vacant(entry) => {
                let mut parser = Parser::new();
                parser
                    .set_language(entry.key())
                    .context("loading tree-sitter grammar")?;
                entry.insert(parser)
            }
        };
        parser.reset();
        parser
            .parse(source, None)
            .context("tree-sitter parse produced no tree")
    })
}

#[cfg(all(test, feature = "tree-sitter"))]
mod tests {
    use super::*;

    fn extract(source: &str, file: &str) -> Extraction {
        extract_symbols(
            Path::new(file),
            source,
            Language::from_path(Path::new(file)).unwrap(),
        )
        .expect("extract")
    }

    fn names(extraction: &Extraction) -> Vec<&str> {
        extraction.symbols.iter().map(|s| s.name.as_str()).collect()
    }

    #[test]
    fn rust_symbols_kinds_and_containers() {
        let source = r#"
const MAX_RETRIES: usize = 3;
mod transport {
    pub fn connect() {}
}
struct Registry { name: String }
enum Mode { Fast, Slow }
trait Flush {
    fn flush(&self);
}
impl Registry {
    fn build(&self) -> u32 { 0 }
}
fn main() {
    let r = Registry;
}
"#;
        let ex = extract(source, "src/lib.rs");
        assert_eq!(
            names(&ex),
            [
                "MAX_RETRIES",
                "transport",
                "connect",
                "Registry",
                "Mode",
                "Flush",
                "flush",
                "build",
                "main"
            ]
        );

        let find = |n: &str| ex.symbols.iter().find(|s| s.name == n).unwrap();
        assert_eq!(find("MAX_RETRIES").kind, SymbolKind::Constant);
        assert_eq!(find("transport").kind, SymbolKind::Module);
        assert_eq!(find("Registry").kind, SymbolKind::Struct);
        assert_eq!(find("Flush").kind, SymbolKind::Trait);
        assert_eq!(find("flush").kind, SymbolKind::Method);
        assert_eq!(find("flush").container.as_deref(), Some("Flush"));
        assert_eq!(find("build").kind, SymbolKind::Method);
        assert_eq!(find("build").container.as_deref(), Some("Registry"));
        assert_eq!(find("main").kind, SymbolKind::Function);
        assert!(find("main").container.is_none());
        assert!(
            find("build")
                .signature
                .as_deref()
                .is_some_and(|s| s.contains("fn build"))
        );

        let refs: Vec<&Occurrence> = ex
            .occurrences
            .iter()
            .filter(|o| o.name == "Registry" && o.role == OccurrenceRole::Reference)
            .collect();
        // Lexical occurrences: the `impl Registry` header type + the use in
        // `main`. Both are useful navigation targets.
        assert_eq!(
            refs.len(),
            2,
            "`Registry` referenced via impl header and use in main"
        );
    }

    #[test]
    fn python_classes_methods_and_references() {
        let source = "\nclass Client:\n    def connect(self):\n        pass\n\ndef helper():\n    c = Client()\n    return c\n";
        let ex = extract(source, "client.py");
        assert_eq!(names(&ex), ["Client", "connect", "helper"]);
        let find = |n: &str| ex.symbols.iter().find(|s| s.name == n).unwrap();
        assert_eq!(find("Client").kind, SymbolKind::Class);
        assert_eq!(find("connect").kind, SymbolKind::Method);
        assert_eq!(find("connect").container.as_deref(), Some("Client"));
        assert_eq!(find("helper").kind, SymbolKind::Function);
        assert!(
            ex.occurrences
                .iter()
                .any(|o| o.name == "Client" && o.role == OccurrenceRole::Reference)
        );
    }

    #[test]
    fn typescript_interfaces_types_enums() {
        let source = "\ninterface Config { port: number; }\ntype Alias = string;\nenum Color { Red }\nclass Service {\n    method(): void {}\n}\nfunction main(): void { const s = new Service(); }\n";
        let ex = extract(source, "app.ts");
        assert_eq!(
            names(&ex),
            ["Config", "Alias", "Color", "Service", "method", "main"]
        );
        let find = |n: &str| ex.symbols.iter().find(|s| s.name == n).unwrap();
        assert_eq!(find("Config").kind, SymbolKind::Interface);
        assert_eq!(find("Alias").kind, SymbolKind::TypeAlias);
        assert_eq!(find("Color").kind, SymbolKind::Enum);
        assert_eq!(find("method").kind, SymbolKind::Method);
        assert_eq!(find("method").container.as_deref(), Some("Service"));
        assert!(
            ex.occurrences
                .iter()
                .any(|o| o.name == "Service" && o.role == OccurrenceRole::Reference)
        );
    }

    #[test]
    fn tsx_uses_tsx_grammar() {
        let source = "\nexport function App() {\n  return <div>hi</div>;\n}\n";
        let ex = extract(source, "app.tsx");
        assert_eq!(names(&ex), ["App"]);
    }

    #[test]
    fn go_types_functions_and_methods() {
        let source = "\npackage main\n\ntype Registry struct{}\ntype Reader interface{}\ntype Handler = int\n\nfunc Build() {}\n\nfunc (r *Registry) Close() error {\n\treturn nil\n}\n\nfunc main() {\n\tr := Registry{}\n}\n";
        let ex = extract(source, "main.go");
        assert_eq!(
            names(&ex),
            ["Registry", "Reader", "Handler", "Build", "Close", "main"]
        );
        let find = |n: &str| ex.symbols.iter().find(|s| s.name == n).unwrap();
        assert_eq!(find("Registry").kind, SymbolKind::Struct);
        assert_eq!(find("Reader").kind, SymbolKind::Interface);
        assert_eq!(find("Handler").kind, SymbolKind::TypeAlias);
        assert_eq!(find("Close").kind, SymbolKind::Method);
        assert_eq!(find("Close").container.as_deref(), Some("Registry"));
        assert!(
            ex.occurrences
                .iter()
                .any(|o| o.name == "Registry" && o.role == OccurrenceRole::Reference)
        );
    }

    #[test]
    fn javascript_functions_classes_methods() {
        let source = "\nfunction helper() {}\nclass Store {\n  load() {}\n}\n";
        let ex = extract(source, "store.js");
        assert_eq!(names(&ex), ["helper", "Store", "load"]);
        let find = |n: &str| ex.symbols.iter().find(|s| s.name == n).unwrap();
        assert_eq!(find("helper").kind, SymbolKind::Function);
        assert_eq!(find("load").kind, SymbolKind::Method);
        assert_eq!(find("load").container.as_deref(), Some("Store"));
    }

    #[test]
    fn symbol_paths_carry_the_caller_provided_path() {
        let source = "fn alpha() {}";
        let ex = extract(source, "src/a/mod.rs");
        assert_eq!(ex.symbols[0].path, "src/a/mod.rs");
        assert_eq!(ex.occurrences[0].path, "src/a/mod.rs");
    }

    #[test]
    fn nested_function_likes_are_functions_not_methods() {
        let source = r#"
impl Registry {
    fn build(&self) {
        fn helper() {}
    }
}
trait Flush {
    fn flush(&self) {
        fn inner() {}
    }
}
"#;
        let ex = extract(source, "nested.rs");
        let find = |n: &str| ex.symbols.iter().find(|s| s.name == n).unwrap();
        assert_eq!(find("build").kind, SymbolKind::Method);
        assert_eq!(find("build").container.as_deref(), Some("Registry"));
        assert_eq!(find("helper").kind, SymbolKind::Function);
        assert!(find("helper").container.is_none());
        assert_eq!(find("flush").kind, SymbolKind::Method);
        assert_eq!(find("flush").container.as_deref(), Some("Flush"));
        assert_eq!(find("inner").kind, SymbolKind::Function);
        assert!(find("inner").container.is_none());
    }

    #[test]
    fn python_nested_def_is_function_not_method() {
        let source =
            "\nclass Client:\n    def connect(self):\n        def retry():\n            pass\n";
        let ex = extract(source, "nested.py");
        let find = |n: &str| ex.symbols.iter().find(|s| s.name == n).unwrap();
        assert_eq!(find("connect").kind, SymbolKind::Method);
        assert_eq!(find("connect").container.as_deref(), Some("Client"));
        assert_eq!(find("retry").kind, SymbolKind::Function);
        assert!(find("retry").container.is_none());
    }
}
