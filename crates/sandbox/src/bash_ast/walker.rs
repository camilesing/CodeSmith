//! Tree walking: turns a tree-sitter-bash syntax tree into [`BashFacts`].
//!
//! Node-type reference (verified against tree-sitter-bash via parse dumps):
//!
//! * `program` children are top-level statements; `;`-separated commands are
//!   direct siblings, while `&&`/`||` sequences wrap in `list` and `|` in
//!   `pipeline`. Operators themselves are anonymous child nodes (`"&&"`,
//!   `"||"`, `"|"`, `";"`, `"&"`).
//! * Redirections attach to a `redirected_statement` (body + `file_redirect` /
//!   `heredoc_redirect` children); a `heredoc_redirect` can itself nest a
//!   `file_redirect` (`cat <<EOF > f`). `file_redirect` may carry a
//!   `file_descriptor` child (`2>`).
//! * Prefix `VAR=value` words are `variable_assignment` children inside
//!   `command`; `declare`/`export` are `declaration_command`.
//! * Expansions are `simple_expansion` (`$VAR`) / `expansion` (`${VAR…}`);
//!   substitutions are `command_substitution` (`$(…)`, backticks) and
//!   `process_substitution` (`<(…)`), and they nest inside `string` and
//!   `concatenation` nodes.

use tree_sitter::{Node, Parser};

use super::facts::{AstParseFailure, BashFacts, CommandSegment, Redirect, SegmentJoin};

/// Parse `source` and walk the tree into facts. Fails closed when the tree
/// contains `ERROR`/`MISSING` nodes.
///
/// The parser is cached per thread: `analyze_command`, `normalize_command`,
/// `split_command_segments`, and the rule matchers all parse the same
/// strings, and `Parser::new` + `set_language` per call dominated that hot
/// path. `Parser` is `Send` but not `Sync`, so a `thread_local` `RefCell`
/// is the shareable shape.
pub(super) fn parse_facts(source: &str) -> Result<BashFacts, AstParseFailure> {
    thread_local! {
        static PARSER: std::cell::RefCell<Parser> = std::cell::RefCell::new({
            let mut parser = Parser::new();
            let language: tree_sitter::Language = tree_sitter_bash::LANGUAGE.into();
            parser
                .set_language(&language)
                .expect("tree-sitter-bash grammar loads");
            parser
        });
    }
    let tree = PARSER.with(|parser| parser.borrow_mut().parse(source, None));
    let Some(tree) = tree else {
        return Err(AstParseFailure {
            first_error_byte: 0,
        });
    };
    let root = tree.root_node();
    if root.has_error() {
        return Err(AstParseFailure {
            first_error_byte: first_error_byte(root),
        });
    }
    let mut walker = Walker {
        source,
        segments: Vec::new(),
        substitution_outside_segments: false,
        nested_depth: 0,
    };
    walker.walk_statement(root, None);
    Ok(BashFacts {
        segments: walker.segments,
        has_command_substitution: walker.substitution_outside_segments,
    })
}

/// Map an anonymous operator node's text to a join kind.
fn join_of_operator(kind: &str) -> Option<SegmentJoin> {
    match kind {
        "&&" => Some(SegmentJoin::And),
        "||" => Some(SegmentJoin::Or),
        "|" => Some(SegmentJoin::Pipe),
        ";" | "\n" => Some(SegmentJoin::Sequence),
        _ => None,
    }
}

/// Kinds that represent one executable statement at any nesting level.
fn is_statement_kind(kind: &str) -> bool {
    matches!(
        kind,
        "command"
            | "list"
            | "pipeline"
            | "redirected_statement"
            | "negated_command"
            | "subshell"
            | "variable_assignment"
            | "declaration_command"
            | "unset_command"
            | "test_command"
            | "for_statement"
            | "while_statement"
            | "until_statement"
            | "if_statement"
            | "case_statement"
            | "function_definition"
    )
}

struct Walker<'a> {
    source: &'a str,
    segments: Vec<CommandSegment>,
    substitution_outside_segments: bool,
    /// Nonzero while walking nested positions (substitution bodies,
    /// assignment values, subshells, compound-statement bodies); emitted
    /// segments are marked `is_nested` so policy matching can reconstruct
    /// the top level while safety analysis still sees them.
    nested_depth: usize,
}

impl<'a> Walker<'a> {
    /// Walk the children of `node` (a statement or compound container),
    /// emitting segments. `pending` is the join the first emitted segment
    /// inherits from the enclosing context.
    fn walk_statement(&mut self, node: Node, pending: Option<SegmentJoin>) {
        let mut pending = pending;
        let mut emitted_here = false;
        let mut cursor = node.walk();
        for child in node.children(&mut cursor) {
            let kind = child.kind();
            if !child.is_named() {
                // Anonymous operator tokens between statements.
                match kind {
                    "&&" | "||" | "|" | ";" | "\n" => pending = join_of_operator(kind),
                    "&" => {
                        if let Some(last) = self.segments.last_mut() {
                            last.is_background = true;
                        }
                        pending = Some(SegmentJoin::Sequence);
                    }
                    _ => {}
                }
                continue;
            }
            if is_statement_kind(kind) {
                // Adjacent statements without an operator between them (`;`
                // terminators may not survive as tree nodes): sequence.
                let join = pending
                    .take()
                    .or_else(|| emitted_here.then_some(SegmentJoin::Sequence));
                self.walk_named_statement(child, join);
                emitted_here = true;
                continue;
            }
            // Compound containers (do_group, clauses, compound_statement,
            // substitution bodies, argument interiors…): commands nested
            // inside join the flow as sequence members. Everything below a
            // top-level statement is nested.
            self.nested_depth += 1;
            self.walk_statement(child, pending.take());
            self.nested_depth -= 1;
        }
    }

    /// Walk a statement whose contents count as *nested* segments.
    fn walk_nested(&mut self, node: Node, join: Option<SegmentJoin>) {
        self.nested_depth += 1;
        self.walk_statement(node, join);
        self.nested_depth -= 1;
    }

    fn walk_named_statement(&mut self, node: Node, join: Option<SegmentJoin>) {
        match node.kind() {
            "command" | "declaration_command" | "unset_command" | "test_command" => {
                self.emit_command(node, join)
            }
            "list" | "pipeline" => self.walk_statement(node, join),
            "subshell" => self.walk_nested(node, join),
            "redirected_statement" => self.walk_redirected(node, join),
            "negated_command" => {
                let before = self.segments.len();
                self.walk_statement(node, join);
                for segment in &mut self.segments[before..] {
                    segment.is_negated = true;
                }
            }
            "variable_assignment" => {
                // A standalone assignment (`x=$(cmd)`) emits no segment of
                // its own; record that it runs a substitution so the safety
                // gate can still see it.
                if self.subtree_has_kind(node, "command_substitution") {
                    self.substitution_outside_segments = true;
                }
                self.walk_nested(node, join);
            }
            // Compound statements (for/while/if/case/function bodies): walk
            // the container; inner commands surface as nested segments.
            _ => self.walk_nested(node, join),
        }
    }

    fn walk_redirected(&mut self, node: Node, join: Option<SegmentJoin>) {
        let mut redirects: Vec<Redirect> = Vec::new();
        let mut has_heredoc = false;
        let mut expansion_in_target = false;
        let mut body: Option<Node> = None;
        let mut cursor = node.walk();
        for child in node.children(&mut cursor) {
            match child.kind() {
                "file_redirect" => {
                    if let Some(redirect) = self.collect_redirect(child) {
                        expansion_in_target |= self.subtree_has_expansion(child);
                        redirects.push(redirect);
                    }
                }
                "heredoc_redirect" => {
                    has_heredoc = true;
                    // `cat <<EOF > f` nests the file redirect inside the
                    // heredoc redirect; the heredoc *body* never contributes
                    // tokens or segments.
                    self.collect_nested_redirects(child, &mut redirects, &mut expansion_in_target);
                }
                _ => body = Some(child),
            }
        }
        let before = self.segments.len();
        if let Some(body_node) = body {
            self.walk_named_statement(body_node, join);
        }
        // Attach the statement's redirects to the first segment it produced.
        if let Some(segment) = self.segments.get_mut(before) {
            segment.redirects.extend(redirects);
            segment.has_heredoc |= has_heredoc;
            segment.expansion_in_redirect_target |= expansion_in_target;
        }
    }

    fn collect_nested_redirects(
        &self,
        heredoc: Node,
        redirects: &mut Vec<Redirect>,
        expansion_in_target: &mut bool,
    ) {
        let mut cursor = heredoc.walk();
        for nested in heredoc.children(&mut cursor) {
            if nested.kind() == "file_redirect"
                && let Some(redirect) = self.collect_redirect(nested)
            {
                *expansion_in_target |= self.subtree_has_expansion(nested);
                redirects.push(redirect);
            }
        }
    }

    /// Emit one simple command segment from a `command`-like node.
    fn emit_command(&mut self, node: Node, join: Option<SegmentJoin>) {
        // `declare -x FOO=bar` / `unset FOO`: the assignment words are the
        // payload of the statement, not a prefix environment.
        let assignments_are_argv = matches!(node.kind(), "declaration_command" | "unset_command");
        let mut segment = CommandSegment {
            join,
            is_nested: self.nested_depth > 0,
            ..CommandSegment::default()
        };
        let mut cursor = node.walk();
        for child in node.children(&mut cursor) {
            // `declaration_command` / `unset_command` carry their leading
            // keyword (`declare`, `export`, `unset`) as an anonymous node —
            // it is the command name, so it belongs in argv.
            if assignments_are_argv && !child.is_named() {
                segment.argv.push(self.raw_text(child));
                continue;
            }
            match child.kind() {
                "variable_assignment" => {
                    segment.has_command_substitution |=
                        self.subtree_has_kind(child, "command_substitution");
                    if assignments_are_argv {
                        segment.argv.push(self.raw_text(child));
                        // `export X=$(cmd)` executes the value like a prefix
                        // assignment does — walk it so the embedded command
                        // flattens into a segment.
                        self.walk_nested(child, None);
                    } else {
                        // Prefix assignment: not part of argv, but a
                        // substitution in its value still executes.
                        segment.assignments.push(self.raw_text(child));
                        self.walk_nested(child, None);
                    }
                }
                "command_name" => {
                    segment.expansion_in_command_name = self.subtree_has_expansion(child);
                    // `$(cmd) args` puts the substitution in command
                    // position: flag it and walk it like any argument, so
                    // the embedded command flattens into a segment.
                    segment.has_command_substitution |=
                        self.subtree_has_kind(child, "command_substitution");
                    segment.has_process_substitution |=
                        self.subtree_has_kind(child, "process_substitution");
                    self.walk_nested(child, None);
                    segment.argv.push(self.token_text(child));
                }
                "file_redirect" => {
                    if let Some(redirect) = self.collect_redirect(child) {
                        segment.expansion_in_redirect_target |= self.subtree_has_expansion(child);
                        segment.redirects.push(redirect);
                    }
                }
                "heredoc_redirect" => {
                    segment.has_heredoc = true;
                    let mut redirects = std::mem::take(&mut segment.redirects);
                    let mut exp = segment.expansion_in_redirect_target;
                    self.collect_nested_redirects(child, &mut redirects, &mut exp);
                    segment.redirects = redirects;
                    segment.expansion_in_redirect_target = exp;
                }
                // Arguments (word/string/raw_string/number/concatenation/
                // expansions/substitutions) resolve to their shell value;
                // substitutions keep raw text and their inner commands
                // flatten to their own segments (they run first, so they are
                // pushed before this segment — execution order).
                _ => {
                    if !child.is_named() {
                        continue;
                    }
                    segment.has_command_substitution |=
                        self.subtree_has_kind(child, "command_substitution");
                    segment.has_process_substitution |=
                        self.subtree_has_kind(child, "process_substitution");
                    segment.argv.push(self.token_text(child));
                    self.walk_nested(child, None);
                }
            }
        }
        self.segments.push(segment);
    }

    /// Extract an output `file_redirect` node into a [`Redirect`]. Returns
    /// `None` for input redirects (`<`), dup forms (`2>&1`), and descriptor
    /// targets — none of them write to a path.
    fn collect_redirect(&self, node: Node) -> Option<Redirect> {
        let mut operator = String::new();
        let mut cursor = node.walk();
        for child in node.children(&mut cursor) {
            if !child.is_named() {
                operator.push_str(&self.raw_text(child));
            }
        }
        if let Some(fd) = node.child_by_field_name("descriptor") {
            operator.insert_str(0, &self.raw_text(fd));
        }
        if !operator.contains('>') {
            return None; // `<`-only redirect: reads, does not write
        }
        let destination = node.child_by_field_name("destination")?;
        let target = self.token_text(destination);
        // Dup redirections (`2>&1`, `>&2`) point at another descriptor, not
        // a path — not a write target. They carry the `&` *after* the `>`;
        // whole-output forms (`&>`, `&>>`) redirect stdout+stderr to a path
        // and must stay visible to write-target analysis.
        if operator.contains(">&") || target.starts_with('&') {
            return None;
        }
        Some(Redirect { operator, target })
    }

    fn subtree_has_kind(&self, node: Node, kind: &str) -> bool {
        let mut stack = vec![node];
        while let Some(node) = stack.pop() {
            if node.kind() == kind {
                return true;
            }
            let mut cursor = node.walk();
            stack.extend(node.children(&mut cursor));
        }
        false
    }

    fn subtree_has_expansion(&self, node: Node) -> bool {
        self.subtree_has_kind(node, "simple_expansion") || self.subtree_has_kind(node, "expansion")
    }

    /// Raw source text of a node (no unquoting).
    fn raw_text(&self, node: Node) -> String {
        node.utf8_text(self.source.as_bytes())
            .unwrap_or_default()
            .to_string()
    }

    /// Resolve a token node to its shell-value string: quotes stripped,
    /// backslash escapes resolved, expansions/substitutions kept as raw text.
    fn token_text(&self, node: Node) -> String {
        match node.kind() {
            "command_name" => {
                // Resolve the underlying word/string/concatenation child so
                // escapes are unquoted (`\rm` → `rm`).
                let mut cursor = node.walk();
                node.children(&mut cursor)
                    .find(|c| c.is_named())
                    .map(|c| self.token_text(c))
                    .unwrap_or_default()
            }
            "word" => unescape_word(&self.raw_text(node)),
            "raw_string" => self
                .raw_text(node)
                .strip_prefix('\'')
                .and_then(|t| t.strip_suffix('\''))
                .unwrap_or(&self.raw_text(node))
                .to_string(),
            "string" => unquote_double(&self.raw_text(node)),
            "concatenation" => {
                let mut cursor = node.walk();
                node.children(&mut cursor)
                    .filter(|c| c.is_named())
                    .map(|c| self.token_text(c))
                    .collect::<String>()
            }
            // Expansions, substitutions, numbers, variables: raw text.
            _ => self.raw_text(node),
        }
    }
}

/// Resolve backslash escapes outside quotes: `\x` → `x` (covers `r\m` → `rm`
/// and `foo\;` → `foo;`). A trailing lone backslash is dropped.
fn unescape_word(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut chars = text.chars();
    while let Some(ch) = chars.next() {
        if ch == '\\' {
            if let Some(escaped) = chars.next() {
                out.push(escaped);
            }
        } else {
            out.push(ch);
        }
    }
    out
}

/// Strip outer double quotes and resolve the escapes that are special inside
/// double quotes (`\"`, `` \` ``, `\$`, `\\`); other backslashes stay literal.
fn unquote_double(text: &str) -> String {
    let inner = text
        .strip_prefix('"')
        .and_then(|t| t.strip_suffix('"'))
        .unwrap_or(text);
    let mut out = String::with_capacity(inner.len());
    let mut chars = inner.chars();
    while let Some(ch) = chars.next() {
        if ch == '\\' {
            match chars.clone().next() {
                Some(next @ ('"' | '`' | '$' | '\\')) => {
                    out.push(next);
                    chars.next();
                }
                _ => out.push('\\'),
            }
        } else {
            out.push(ch);
        }
    }
    out
}

/// Byte offset of the first `ERROR`/`MISSING` node in the tree.
fn first_error_byte(node: Node) -> usize {
    if node.is_error() || node.is_missing() {
        return node.start_byte();
    }
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        if child.is_error() || child.is_missing() {
            return child.start_byte();
        }
        let found = first_error_byte(child);
        if found < child.end_byte() {
            return found;
        }
    }
    node.end_byte()
}
