//! The remappable key map: one table, two input layers.
//!
//! [`ActionId`] names every action a key can trigger (the `app.*` table in
//! [`ACTIONS`]); a chord notation (`Ctrl+O`, `Alt+Shift+T`, `Shift+Tab`,
//! `Ctrl+X Ctrl+K`) parses into [`Chord`]s; [`Keymap`] is the compiled-in
//! default table overlaid with the user's `~/.codesmith/keybindings.toml`.
//! Both input layers read the one process-wide table [`active`] returns:
//! the prompt layer generates its rustyline bindings from it
//! ([`Keymap::prompt_events`]) and the mid-turn decoder prefix-matches its
//! byte patterns ([`Keymap::turn_patterns`]).
//!
//! The file is a flat TOML mapping of action id to one chord string or an
//! array of them; an empty array disables the action:
//!
//! ```toml
//! app.tools.expand = "Ctrl+O"
//! app.thinking.cycle = ["Alt+Shift+T", "Ctrl+X Ctrl+T"]
//! app.tasks.toggle = []          # disable
//! ```
//!
//! Chord names are case-insensitive. Malformed content — an unknown action
//! id, an unparsable chord, one chord claimed by two actions in the same
//! layer — fails loud at REPL startup with the file's path (the
//! `capabilities.toml` pattern); a missing file is the defaults.
//! `CODESMITH_KEYBINDINGS_FILE` points elsewhere.
//!
//! Known limitations (recorded beside the behaviour it owns):
//!
//! - The prompt layer (rustyline 14, no kitty keyboard protocol) cannot
//!   distinguish `Ctrl+Enter` from `Enter`, `Shift+Enter` from `Enter`, or
//!   `Ctrl+Shift+<key>` from `Ctrl+<key>`. Binding one of those to a
//!   prompt-scoped action loads with a warning and never fires at the
//!   prompt; the mid-turn decoder honours `Ctrl+Enter` (`CSI 13;5u`) where
//!   the terminal sends it. `Ctrl+Shift+<letter>` is rejected outright —
//!   no layer can ever see it.
//! - `Escape`, `Ctrl+C`, `Ctrl+D`, `?`, `Tab` and the readline editing
//!   keys are fixed: they are not in the table and cannot be remapped.
//! - A mid-turn action chord takes precedence over the composer's own use
//!   of the same bytes — binding an action to `Ctrl+E` costs the composer
//!   its Home key while a turn runs.
//! - A lone trailing `Escape` stays Escape even when Alt chords are bound:
//!   the decoder does not wait for a second byte that may never come, so a
//!   rare split Alt-chord read fires the interrupt instead.
//! - No profile overlay (v1): one file per install.
//! - Pasted images (`app.clipboard.pasteImage`) work at the prompt only;
//!   the mid-turn composer has no attachment path.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use rustyline::{Event, KeyCode, KeyEvent, Modifiers};

/// Where a binding can fire.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Scope {
    /// The idle prompt (rustyline).
    Prompt,
    /// While a turn runs (the byte decoder).
    Turn,
    /// Both layers.
    Both,
}

impl Scope {
    /// Whether the prompt layer should register this action.
    pub fn fires_at_prompt(self) -> bool {
        !matches!(self, Scope::Turn)
    }
    /// Whether the mid-turn decoder should recognize this action.
    pub fn fires_mid_turn(self) -> bool {
        !matches!(self, Scope::Prompt)
    }
}

/// The key of a chord. Letters carry their case: `Shift` on a letter folds
/// into the uppercase form (what a terminal actually sends), so `Alt+T`
/// (`t`) and `Alt+Shift+T` (`T`) are different chords.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Key {
    Char(char),
    Enter,
    Tab,
    Esc,
    Backspace,
    Delete,
    Insert,
    Up,
    Down,
    Left,
    Right,
    Home,
    End,
    PageUp,
    PageDown,
    F(u8),
}

/// One chord: a key plus modifiers. `shift` survives only on non-letter
/// keys — on letters it is folded into the character's case at parse time.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Chord {
    pub key: Key,
    pub ctrl: bool,
    pub alt: bool,
    pub shift: bool,
}

/// One user binding: a single chord, or a two-key sequence (`Ctrl+X Ctrl+K`).
pub type Binding = Vec<Chord>;

/// Every action a key can trigger. The dotted names are the config-file
/// keys; `ACTIONS` owns the defaults, scopes, and help lines.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ActionId {
    ModeCycle,
    ToolsExpand,
    TasksToggle,
    StopAll,
    ThinkingCycle,
    ModelCycle,
    ThinkingToggle,
    EditorExternal,
    PasteImage,
    CopyPrompt,
    CopyLine,
    Retry,
    DraftStash,
    PromptNewline,
    PromptRecall,
    QueueSendNow,
    QueueBackground,
}

/// Static metadata for one action.
pub struct ActionSpec {
    pub id: ActionId,
    pub name: &'static str,
    pub default: &'static [&'static str],
    pub scope: Scope,
    pub help: &'static str,
}

/// The action table, in `/keybindings` print order.
pub fn action_specs() -> &'static [ActionSpec] {
    ACTIONS
}

/// The action table. Order is the order `/keybindings` prints.
const ACTIONS: &[ActionSpec] = &[
    ActionSpec {
        id: ActionId::ModeCycle,
        name: "app.mode.cycle",
        default: &["Shift+Tab"],
        scope: Scope::Both,
        help: "cycle the run mode (agent · plan · yolo)",
    },
    ActionSpec {
        id: ActionId::ToolsExpand,
        name: "app.tools.expand",
        default: &["Ctrl+O"],
        scope: Scope::Both,
        help: "toggle collapsed/expanded tool-call detail",
    },
    ActionSpec {
        id: ActionId::TasksToggle,
        name: "app.tasks.toggle",
        default: &["Ctrl+T"],
        scope: Scope::Both,
        help: "toggle the task checklist",
    },
    ActionSpec {
        id: ActionId::ThinkingCycle,
        name: "app.thinking.cycle",
        default: &["Alt+Shift+T"],
        scope: Scope::Both,
        help: "cycle the thinking level (off · high · max)",
    },
    ActionSpec {
        id: ActionId::StopAll,
        name: "app.stopAll",
        default: &["Ctrl+X Ctrl+K"],
        scope: Scope::Both,
        help: "stop all background work (press twice)",
    },
    ActionSpec {
        id: ActionId::ModelCycle,
        name: "app.model.cycle",
        default: &["Alt+P"],
        scope: Scope::Prompt,
        help: "switch model (session default ↔ configured default)",
    },
    ActionSpec {
        id: ActionId::ThinkingToggle,
        name: "app.thinking.toggle",
        default: &["Alt+T"],
        scope: Scope::Prompt,
        help: "toggle extended thinking (off ↔ high)",
    },
    ActionSpec {
        id: ActionId::EditorExternal,
        name: "app.editor.external",
        default: &["Ctrl+G", "Ctrl+X Ctrl+E"],
        scope: Scope::Prompt,
        help: "edit the prompt in $VISUAL/$EDITOR",
    },
    ActionSpec {
        id: ActionId::PasteImage,
        name: "app.clipboard.pasteImage",
        default: &["Ctrl+V"],
        scope: Scope::Prompt,
        help: "paste a clipboard image as an attachment",
    },
    ActionSpec {
        id: ActionId::CopyPrompt,
        name: "app.clipboard.copyPrompt",
        default: &["Alt+Shift+C"],
        scope: Scope::Prompt,
        help: "copy the whole prompt draft",
    },
    ActionSpec {
        id: ActionId::CopyLine,
        name: "app.clipboard.copyLine",
        default: &["Alt+Shift+L"],
        scope: Scope::Prompt,
        help: "copy the line under the cursor",
    },
    ActionSpec {
        id: ActionId::Retry,
        name: "app.retry",
        default: &["F5", "Alt+R"],
        scope: Scope::Prompt,
        help: "retry the last request (same as /retry)",
    },
    ActionSpec {
        id: ActionId::DraftStash,
        name: "app.draft.stash",
        default: &["Ctrl+S"],
        scope: Scope::Prompt,
        help: "stash the draft · press again on an empty prompt",
    },
    ActionSpec {
        id: ActionId::PromptNewline,
        name: "app.prompt.newline",
        default: &["Ctrl+J", "Alt+Enter"],
        scope: Scope::Prompt,
        help: "insert a newline (multiline prompt)",
    },
    ActionSpec {
        id: ActionId::PromptRecall,
        name: "app.prompt.recall",
        default: &["Up"],
        scope: Scope::Prompt,
        help: "recall the cleared draft, then queued messages, then history",
    },
    ActionSpec {
        id: ActionId::QueueSendNow,
        name: "app.queue.sendNow",
        default: &["Ctrl+S", "Ctrl+Enter", "Ctrl+X Ctrl+S"],
        scope: Scope::Turn,
        help: "send what is queued (and composed) to the running turn now",
    },
    ActionSpec {
        id: ActionId::QueueBackground,
        name: "app.queue.background",
        default: &["Ctrl+B"],
        scope: Scope::Turn,
        help: "send the running shell to the background",
    },
];

impl ActionId {
    /// The spec behind an action — every variant has one.
    fn spec(self) -> &'static ActionSpec {
        ACTIONS
            .iter()
            .find(|spec| spec.id == self)
            .expect("every ActionId has a spec")
    }

    /// The dotted config-file name.
    fn name(self) -> &'static str {
        self.spec().name
    }

    /// Parse a config-file name, or `None` when it names no action.
    fn from_name(name: &str) -> Option<ActionId> {
        ACTIONS
            .iter()
            .find(|spec| spec.name == name)
            .map(|spec| spec.id)
    }

    /// Every valid config-file key, comma-joined (for error messages).
    fn all_names_joined() -> String {
        ACTIONS
            .iter()
            .map(|spec| spec.name)
            .collect::<Vec<_>>()
            .join(", ")
    }
}

/// The effective table: defaults overlaid with the user's file.
#[derive(Debug)]
pub struct Keymap {
    /// Every action's effective bindings (empty = disabled or unbound).
    bindings: HashMap<ActionId, Vec<Binding>>,
    /// Actions whose bindings came from the user file (the `*` marker).
    remapped: HashSet<ActionId>,
    /// Load-time findings that do not stop startup (undecodable chords).
    warnings: Vec<String>,
    /// The file this table was overlaid from, when there was one.
    source: Option<PathBuf>,
    /// Cached prompt-layer registrations, longest chord sequence first.
    prompt_table: Vec<(Event, ActionId)>,
    /// Cached mid-turn byte patterns, longest first (prefix precedence).
    turn_table: Vec<(Vec<u8>, ActionId)>,
}

impl Default for Keymap {
    fn default() -> Self {
        let mut map = Keymap {
            bindings: HashMap::new(),
            remapped: HashSet::new(),
            warnings: Vec::new(),
            source: None,
            prompt_table: Vec::new(),
            turn_table: Vec::new(),
        };
        for spec in ACTIONS {
            let defaults = spec
                .default
                .iter()
                .map(|text| parse_binding(text).expect("a compiled-in default parses"))
                .collect();
            map.bindings.insert(spec.id, defaults);
        }
        // The compiled-in table is valid by construction; the expect keeps
        // a drifted default from silently producing an empty key map.
        map.finish().expect("the compiled-in defaults validate");
        map
    }
}

impl Keymap {
    /// Overlay user bindings from `text` (the keybindings.toml content).
    /// Validation and the derived tables land in [`Keymap::finish`].
    pub fn overlay(&mut self, text: &str, path: &Path) -> Result<(), String> {
        let table: toml::Table =
            toml::from_str(text).map_err(|err| format!("parse {}: {err}", path.display()))?;
        // TOML dotted keys (`app.tools.expand = …`) parse into nested
        // tables; flatten them back into dotted action paths. A `[section]`
        // header is indistinguishable from a dotted key after parsing, so
        // both spellings are accepted.
        let mut entries: Vec<(String, toml::Value)> = Vec::new();
        flatten_table(&table, String::new(), &mut entries);
        for (key, value) in entries {
            let id = ActionId::from_name(&key).ok_or_else(|| {
                format!(
                    "unknown action {key:?} in {} — valid actions: {}",
                    path.display(),
                    ActionId::all_names_joined()
                )
            })?;
            let chord_texts: Vec<String> = match value {
                toml::Value::String(text) => vec![text],
                toml::Value::Array(items) => {
                    let mut texts = Vec::new();
                    for item in items {
                        let Some(text) = item.as_str() else {
                            return Err(format!("{key}: each chord must be a string, got {item}"));
                        };
                        texts.push(text.to_string());
                    }
                    texts
                }
                other => {
                    return Err(format!(
                        "{key}: expected a chord string or an array of them, got {other}"
                    ));
                }
            };
            let mut bindings = Vec::new();
            for text in chord_texts {
                let trimmed = text.trim();
                if trimmed.is_empty() {
                    return Err(format!(
                        "{key}: an empty string is not a chord — use [] to disable the action"
                    ));
                }
                bindings.push(
                    parse_binding(trimmed)
                        .map_err(|err| format!("{key} in {}: {err}", path.display()))?,
                );
            }
            // An empty array stores zero bindings: the action is disabled.
            self.bindings.insert(id, bindings);
            self.remapped.insert(id);
        }
        self.source = Some(path.to_path_buf());
        Ok(())
    }

    /// Validate the table and build the two layer tables. Errors abort the
    /// load (fail loud); warnings survive into [`Keymap::warnings`].
    pub fn finish(&mut self) -> Result<(), String> {
        for spec in ACTIONS {
            let Some(bindings) = self.bindings.get(&spec.id) else {
                continue;
            };
            for binding in bindings {
                for chord in binding {
                    if reserved_for_typing(*chord) {
                        return Err(format!(
                            "{} = {:?}: an action chord needs a modifier (or a non-printing \
                             key) — plain keys belong to typing",
                            spec.name,
                            notation(binding)
                        ));
                    }
                }
            }
        }
        // One chord, one action, per layer: the second claim on a chord the
        // first silently shadows, so it is refused while both can be named.
        let mut prompt_seen: HashMap<Binding, ActionId> = HashMap::new();
        let mut turn_seen: HashMap<Binding, ActionId> = HashMap::new();
        self.prompt_table.clear();
        self.turn_table.clear();
        for spec in ACTIONS {
            let Some(bindings) = self.bindings.get(&spec.id) else {
                continue;
            };
            for binding in bindings {
                if spec.scope.fires_at_prompt() {
                    if let Some(other) = prompt_seen.insert(binding.clone(), spec.id)
                        && other != spec.id
                    {
                        return Err(format!(
                            "{} and {} are both bound to {} at the prompt",
                            other.name(),
                            spec.name,
                            notation(binding)
                        ));
                    }
                    match prompt_event(binding) {
                        Some(event) => self.prompt_table.push((event, spec.id)),
                        None => self.warnings.push(format!(
                            "{} = {:?} — the prompt layer cannot decode this chord \
                             (rustyline has no kitty keyboard protocol); it will not fire there",
                            spec.name,
                            notation(binding)
                        )),
                    }
                }
                if spec.scope.fires_mid_turn() {
                    if let Some(other) = turn_seen.insert(binding.clone(), spec.id)
                        && other != spec.id
                    {
                        return Err(format!(
                            "{} and {} are both bound to {} while a turn runs",
                            other.name(),
                            spec.name,
                            notation(binding)
                        ));
                    }
                    match turn_pattern(binding) {
                        Some(pattern) => self.turn_table.push((pattern, spec.id)),
                        None => self.warnings.push(format!(
                            "{} = {:?} — the mid-turn layer cannot decode this chord; \
                             it will not fire while a turn runs",
                            spec.name,
                            notation(binding)
                        )),
                    }
                }
            }
        }
        // Longest first, so a two-key sequence beats its one-key prefix.
        // (The prompt table needs no order: rustyline registers each event
        // independently and resolves sequence prefixes in its own keymap.)
        self.turn_table
            .sort_by(|a, b| b.0.len().cmp(&a.0.len()).then(a.0.cmp(&b.0)));
        Ok(())
    }

    /// The prompt-layer registrations this table implies.
    pub fn prompt_events(&self) -> &[(Event, ActionId)] {
        &self.prompt_table
    }

    /// The mid-turn byte patterns this table implies, longest first.
    pub fn turn_patterns(&self) -> &[(Vec<u8>, ActionId)] {
        &self.turn_table
    }

    /// An action's effective bindings (empty = disabled).
    pub fn bindings_for(&self, id: ActionId) -> &[Binding] {
        self.bindings.get(&id).map(Vec::as_slice).unwrap_or(&[])
    }

    /// Whether the user's file set this action's bindings.
    pub fn is_remapped(&self, id: ActionId) -> bool {
        self.remapped.contains(&id)
    }

    /// All of an action's chords in display notation, `"a / b"`.
    pub fn labels(&self, id: ActionId) -> String {
        let bindings = self.bindings_for(id);
        if bindings.is_empty() {
            return "(disabled)".to_string();
        }
        bindings
            .iter()
            .map(|b| notation(b))
            .collect::<Vec<_>>()
            .join(" / ")
    }

    /// The first chord in display notation — for prose that names one key.
    /// `(disabled)` when the action has no chords.
    pub fn first_label(&self, id: ActionId) -> String {
        let bindings = self.bindings_for(id);
        match bindings.first() {
            Some(binding) => notation(binding),
            None => "(disabled)".to_string(),
        }
    }

    /// Load-time warnings (undecodable chords), for the loader to print.
    pub fn warnings(&self) -> &[String] {
        &self.warnings
    }

    /// The file this table was overlaid from, when there was one.
    pub fn source_path(&self) -> Option<&Path> {
        self.source.as_deref()
    }
}

/// A chord that must stay available for typing and submitting: unmodified
/// printable keys, Enter, Escape, Tab, Backspace.
fn reserved_for_typing(chord: Chord) -> bool {
    match chord.key {
        Key::Char(_) | Key::Enter | Key::Esc | Key::Tab | Key::Backspace => {
            !(chord.ctrl || chord.alt || chord.shift)
        }
        // Delete, Insert, arrows, Home/End/PageUp/Down and F-keys never
        // insert text, so they may act unmodified.
        _ => false,
    }
}

/// Flatten parsed TOML's nested tables back into dotted action paths.
fn flatten_table(table: &toml::Table, prefix: String, out: &mut Vec<(String, toml::Value)>) {
    for (key, value) in table {
        let path = if prefix.is_empty() {
            key.clone()
        } else {
            format!("{prefix}.{key}")
        };
        match value {
            toml::Value::Table(nested) => flatten_table(nested, path, out),
            leaf => out.push((path, leaf.clone())),
        }
    }
}

/// Parse one binding notation: one chord, or two space-separated chords.
fn parse_binding(text: &str) -> Result<Binding, String> {
    let mut chords = Vec::new();
    for token in text.split_whitespace() {
        if chords.len() == 2 {
            return Err(format!("{text:?}: at most two chords in a sequence"));
        }
        chords.push(parse_chord(token)?);
    }
    match chords.len() {
        1 | 2 => Ok(chords),
        _ => Err(format!("{text:?}: no chords in the binding")),
    }
}

/// Parse one chord token (`Ctrl+O`, `Alt+Shift+T`, `F5`).
fn parse_chord(token: &str) -> Result<Chord, String> {
    let mut ctrl = false;
    let mut alt = false;
    let mut shift = false;
    let mut key: Option<Key> = None;
    for part in token.split('+') {
        if part.is_empty() {
            return Err(format!("{token:?}: empty segment in the chord"));
        }
        match part.to_ascii_lowercase().as_str() {
            "ctrl" | "control" => ctrl = true,
            "alt" | "option" | "opt" | "meta" => alt = true,
            "shift" => shift = true,
            "cmd" | "super" | "win" | "hyper" => {
                return Err(format!(
                    "{token:?}: the terminal cannot report the Cmd/Super modifier"
                ));
            }
            _ => {
                if key.is_some() {
                    return Err(format!("{token:?}: only one key per chord"));
                }
                key =
                    Some(parse_key(part).map_err(|_| format!("{token:?}: unknown key {part:?}"))?);
            }
        }
    }
    let Some(mut key) = key else {
        return Err(format!("{token:?}: no key in the chord"));
    };
    // Fold Shift into a letter's case (what the terminal sends); Ctrl on a
    // letter is the lowercase byte. Ctrl+Shift+letter is byte-identical to
    // Ctrl+letter, so it is refused rather than silently half-honoured.
    if let Key::Char(c) = key
        && c.is_ascii_alphabetic()
    {
        if ctrl && shift {
            return Err(format!(
                "{token:?}: Ctrl+Shift+letter is indistinguishable from Ctrl+letter \
                 — drop the Shift"
            ));
        }
        let folded = if shift {
            c.to_ascii_uppercase()
        } else {
            c.to_ascii_lowercase()
        };
        key = Key::Char(folded);
        shift = false;
    }
    if let Key::Char(c) = key
        && ctrl
    {
        let folded = c.to_ascii_lowercase();
        key = Key::Char(folded);
    }
    Ok(Chord {
        key,
        ctrl,
        alt,
        shift,
    })
}

/// Parse the key segment of a chord token.
fn parse_key(part: &str) -> Result<Key, String> {
    if part.len() == 1 {
        return Ok(Key::Char(part.chars().next().expect("one char")));
    }
    let key = match part.to_ascii_lowercase().as_str() {
        "enter" | "return" => Key::Enter,
        "tab" => Key::Tab,
        "esc" | "escape" => Key::Esc,
        "backspace" | "bs" => Key::Backspace,
        "delete" | "del" => Key::Delete,
        "insert" | "ins" => Key::Insert,
        "up" => Key::Up,
        "down" => Key::Down,
        "left" => Key::Left,
        "right" => Key::Right,
        "home" => Key::Home,
        "end" => Key::End,
        "pageup" | "pgup" => Key::PageUp,
        "pagedown" | "pgdn" => Key::PageDown,
        "space" => Key::Char(' '),
        _ => {
            let Some(number) = part
                .to_ascii_lowercase()
                .strip_prefix('f')
                .and_then(|digits| digits.parse::<u8>().ok())
            else {
                return Err(part.to_string());
            };
            if !(1..=12).contains(&number) {
                return Err(part.to_string());
            }
            Key::F(number)
        }
    };
    Ok(key)
}

/// The rustyline event for one chord, or `None` when the prompt layer
/// cannot decode it (Enter with Ctrl or Shift).
fn key_event(chord: Chord) -> Option<KeyEvent> {
    let Chord {
        key,
        ctrl,
        alt,
        shift,
    } = chord;
    if matches!(key, Key::Enter) && (ctrl || shift) {
        return None;
    }
    // Shift+Tab is what rustyline calls BackTab (what the terminal sends
    // as `CSI Z` decodes to BackTab with no modifiers), so the Shift bit
    // is expressed by the key itself and dropped from the modifier set.
    if matches!(key, Key::Tab) && shift {
        let mut modifiers = Modifiers::NONE;
        if ctrl {
            modifiers.insert(Modifiers::CTRL);
        }
        if alt {
            modifiers.insert(Modifiers::ALT);
        }
        return Some(KeyEvent(KeyCode::BackTab, modifiers));
    }
    let mut modifiers = Modifiers::NONE;
    if ctrl {
        modifiers.insert(Modifiers::CTRL);
    }
    if alt {
        modifiers.insert(Modifiers::ALT);
    }
    if shift {
        modifiers.insert(Modifiers::SHIFT);
    }
    let code = match key {
        Key::Char(c) => KeyCode::Char(c),
        Key::Enter => KeyCode::Enter,
        Key::Tab => KeyCode::Tab,
        Key::Esc => KeyCode::Esc,
        Key::Backspace => KeyCode::Backspace,
        Key::Delete => KeyCode::Delete,
        Key::Insert => KeyCode::Insert,
        Key::Up => KeyCode::Up,
        Key::Down => KeyCode::Down,
        Key::Left => KeyCode::Left,
        Key::Right => KeyCode::Right,
        Key::Home => KeyCode::Home,
        Key::End => KeyCode::End,
        Key::PageUp => KeyCode::PageUp,
        Key::PageDown => KeyCode::PageDown,
        Key::F(number) => KeyCode::F(number),
    };
    Some(KeyEvent(code, modifiers))
}

/// The prompt-layer registration for a binding (a one- or two-key event).
fn prompt_event(binding: &[Chord]) -> Option<Event> {
    let mut events = Vec::with_capacity(binding.len());
    for chord in binding {
        events.push(key_event(*chord)?);
    }
    Some(Event::KeySeq(events))
}

/// The mid-turn byte pattern for a chord, or `None` when the decoder has no
/// spelling for it (F-keys, modified arrows, Shift+Enter, …).
fn chord_pattern(chord: Chord) -> Option<Vec<u8>> {
    match chord.key {
        Key::Char(c) if chord.ctrl && c.is_ascii_lowercase() => Some(vec![c as u8 - b'a' + 1]),
        Key::Char(c) if chord.alt && c.is_ascii() && !c.is_ascii_control() => {
            Some(vec![0x1b, c as u8])
        }
        // Kitty-protocol Ctrl+Enter, the only modified Enter terminals in
        // the wild spell for us mid-turn.
        Key::Enter if chord.ctrl => Some(b"\x1b[13;5u".to_vec()),
        // Shift+Tab: the legacy `CSI Z` every terminal sends.
        Key::Tab if chord.shift => Some(b"\x1b[Z".to_vec()),
        _ => None,
    }
}

/// The mid-turn byte pattern for a binding; a two-key sequence is its two
/// ctrl-letter bytes in a row.
fn turn_pattern(binding: &[Chord]) -> Option<Vec<u8>> {
    let mut pattern = Vec::new();
    for chord in binding {
        pattern.extend(chord_pattern(*chord)?);
    }
    (1..=2).contains(&binding.len()).then_some(pattern)
}

/// Display notation for one binding.
fn notation(binding: &[Chord]) -> String {
    binding
        .iter()
        .map(|chord| chord_notation(*chord))
        .collect::<Vec<_>>()
        .join(" ")
}

/// Display notation for one chord — the parse rules inverted.
fn chord_notation(chord: Chord) -> String {
    let Chord {
        key,
        ctrl,
        alt,
        shift,
    } = chord;
    let mut parts: Vec<String> = Vec::new();
    if ctrl {
        parts.push("Ctrl".to_string());
    }
    if alt {
        parts.push("Alt".to_string());
    }
    let letter_shift = matches!(key, Key::Char(c) if c.is_ascii_uppercase());
    if shift || letter_shift {
        parts.push("Shift".to_string());
    }
    let key_name = match key {
        Key::Char(c) => {
            if c.is_ascii_alphabetic() {
                // Letters display uppercase (`Alt+T`, not `Alt+t`); the
                // Shift prefix above is what separates `Alt+Shift+T`.
                c.to_ascii_uppercase().to_string()
            } else if c.is_ascii_graphic() || c == ' ' {
                c.to_string()
            } else {
                format!("\\u{:02x}", c as u32)
            }
        }
        Key::Enter => "Enter".to_string(),
        Key::Tab => "Tab".to_string(),
        Key::Esc => "Esc".to_string(),
        Key::Backspace => "Backspace".to_string(),
        Key::Delete => "Delete".to_string(),
        Key::Insert => "Insert".to_string(),
        Key::Up => "Up".to_string(),
        Key::Down => "Down".to_string(),
        Key::Left => "Left".to_string(),
        Key::Right => "Right".to_string(),
        Key::Home => "Home".to_string(),
        Key::End => "End".to_string(),
        Key::PageUp => "PageUp".to_string(),
        Key::PageDown => "PageDown".to_string(),
        Key::F(number) => format!("F{number}"),
    };
    parts.push(key_name);
    parts.join("+")
}

/// Where the keybindings file lives: `$CODESMITH_KEYBINDINGS_FILE`, else
/// `keybindings.toml` under the codesmith home.
fn keybindings_path() -> Result<PathBuf, String> {
    if let Ok(path) = std::env::var(KEYBINDINGS_FILE_ENV)
        && !path.trim().is_empty()
    {
        return Ok(PathBuf::from(path));
    }
    codesmith_config::ensure_state_dir(".")
        .map(|dir| dir.join(KEYBINDINGS_FILE_NAME))
        .map_err(|err| format!("cannot resolve the state dir for {KEYBINDINGS_FILE_NAME}: {err:#}"))
}

const KEYBINDINGS_FILE_ENV: &str = "CODESMITH_KEYBINDINGS_FILE";
const KEYBINDINGS_FILE_NAME: &str = "keybindings.toml";

/// The process-wide table. [`load_active`] sets it once at REPL startup;
/// before that (and in tests) it is the compiled-in defaults.
static ACTIVE: OnceLock<Keymap> = OnceLock::new();

/// The active keymap: the loaded table, or the defaults when nothing was
/// loaded (tests, non-REPL entry points).
pub fn active() -> &'static Keymap {
    static DEFAULTS: OnceLock<Keymap> = OnceLock::new();
    ACTIVE
        .get()
        .unwrap_or_else(|| DEFAULTS.get_or_init(Keymap::default))
}

/// Read and validate a keybindings file. `Ok(None)` is the normal first
/// run (no file: defaults); anything malformed is `Err` with the path.
pub fn load_from(path: &Path) -> Result<Option<Keymap>, String> {
    let text = match std::fs::read_to_string(path) {
        Ok(text) => text,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(err) => return Err(format!("read {}: {err}", path.display())),
    };
    let mut map = Keymap::default();
    map.overlay(&text, path)?;
    map.finish()
        .map_err(|err| format!("keybindings file invalid — {err} ({})", path.display()))?;
    Ok(Some(map))
}

/// Load the user's keybindings into the process-wide table. A missing file
/// keeps the defaults; anything malformed fails loud with the file's path.
/// Chords a layer cannot decode warn on stderr but load.
pub fn load_active() -> Result<(), String> {
    let path = keybindings_path()?;
    let Some(map) = load_from(&path)? else {
        return Ok(());
    };
    for warning in map.warnings() {
        eprintln!("warning: keybindings.toml: {warning}");
    }
    let _ = ACTIVE.set(map);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn chord(text: &str) -> Chord {
        parse_chord(text).expect("test chord parses")
    }

    fn loaded(text: &str) -> Keymap {
        let mut map = Keymap::default();
        map.overlay(text, Path::new("/tmp/keybindings.toml"))
            .expect("test overlay loads");
        map.finish().expect("test overlay validates");
        map
    }

    #[test]
    fn chord_parsing_is_case_insensitive_and_folds_shift_into_case() {
        assert_eq!(
            chord("ctrl+O"),
            Chord {
                key: Key::Char('o'),
                ctrl: true,
                alt: false,
                shift: false
            }
        );
        assert_eq!(
            chord("ALT+p"),
            Chord {
                key: Key::Char('p'),
                ctrl: false,
                alt: true,
                shift: false
            }
        );
        assert_eq!(
            chord("Alt+Shift+T"),
            Chord {
                key: Key::Char('T'),
                ctrl: false,
                alt: true,
                shift: false
            },
            "Shift folds into the letter's case so Alt+T and Alt+Shift+T differ"
        );
        assert_eq!(
            chord("shift+tab"),
            Chord {
                key: Key::Tab,
                ctrl: false,
                alt: false,
                shift: true
            }
        );
        assert_eq!(
            chord("f5"),
            Chord {
                key: Key::F(5),
                ctrl: false,
                alt: false,
                shift: false
            }
        );
        assert_eq!(
            chord("Ctrl+Backspace"),
            Chord {
                key: Key::Backspace,
                ctrl: true,
                alt: false,
                shift: false
            }
        );
    }

    #[test]
    fn notation_round_trips_every_compiled_in_default() {
        for spec in ACTIONS {
            for text in spec.default {
                let binding = parse_binding(text).expect("default parses");
                assert_eq!(
                    &notation(&binding),
                    text,
                    "{} drifted from its notation",
                    spec.name
                );
            }
        }
    }

    #[test]
    fn bad_chords_are_named_errors() {
        assert!(
            parse_chord("Ctrl+Shift+O").is_err(),
            "indistinguishable from Ctrl+O"
        );
        assert!(parse_chord("Cmd+P").is_err(), "no Cmd modifier exists");
        assert!(parse_chord("Ctrl").is_err(), "no key");
        assert!(parse_chord("Ctrl+O+P").is_err(), "two keys");
        assert!(parse_chord("Ctrl+NotAKey").is_err());
        assert!(
            parse_binding("Ctrl+X Ctrl+K Ctrl+J").is_err(),
            "three-key sequences"
        );
    }

    #[test]
    fn overlay_remaps_disables_and_rejects_unknown_actions() {
        let map = loaded("app.tasks.toggle = \"Ctrl+Y\"\napp.tools.expand = []\n");
        assert_eq!(
            map.bindings_for(ActionId::TasksToggle),
            &[parse_binding("Ctrl+Y").unwrap()]
        );
        assert!(
            map.bindings_for(ActionId::ToolsExpand).is_empty(),
            "[] disables"
        );
        assert!(map.is_remapped(ActionId::TasksToggle));
        assert!(!map.is_remapped(ActionId::ModeCycle));
        assert_eq!(map.labels(ActionId::ToolsExpand), "(disabled)");

        let mut bad = Keymap::default();
        let err = bad
            .overlay("app.mode.toggle = \"Ctrl+Y\"", Path::new("/tmp/k.toml"))
            .unwrap_err();
        assert!(
            err.contains("app.mode.toggle") && err.contains("app.mode.cycle"),
            "{err}"
        );
    }

    #[test]
    fn plain_typing_keys_are_reserved_for_actions() {
        // The reserved-key check runs in finish(), where it can name the
        // action — overlay only parses.
        for chord in ["\"q\"", "\"Enter\"", "\"Esc\""] {
            let mut map = Keymap::default();
            map.overlay(&format!("app.retry = {chord}",), Path::new("/tmp/k.toml"))
                .unwrap();
            let err = map.finish().unwrap_err();
            assert!(err.contains("needs a modifier"), "{chord}: {err}");
        }
        // A bare space trims to nothing and is refused with its own error.
        let mut map = Keymap::default();
        let err = map
            .overlay("app.retry = \" \"", Path::new("/tmp/k.toml"))
            .unwrap_err();
        assert!(err.contains("empty string"), "{err}");
        // Unmodified non-printing keys are fine (recall is plain Up).
        assert!(
            loaded("app.prompt.recall = \"PageUp\"")
                .bindings_for(ActionId::PromptRecall)
                .len()
                == 1
        );
    }

    #[test]
    fn one_chord_one_action_per_layer() {
        let mut map = Keymap::default();
        map.overlay("app.thinking.cycle = \"Ctrl+O\"", Path::new("/tmp/k.toml"))
            .unwrap();
        let err = map.finish().unwrap_err();
        assert!(
            err.contains("app.tools.expand") && err.contains("at the prompt"),
            "{err}"
        );
        // Across layers is fine: a turn-only action may take a chord a
        // prompt-only action owns (Ctrl+S already works this way — stash
        // at the prompt, send-now mid-turn).
        assert!(
            loaded("app.queue.background = \"Alt+P\"")
                .turn_patterns()
                .iter()
                .any(|(pattern, id)| pattern == &vec![0x1b, b'p']
                    && id == &ActionId::QueueBackground)
        );
    }

    #[test]
    fn defaults_generate_the_layers_the_repl_registers_today() {
        let map = Keymap::default();
        // Prompt: the mode ring, detail, tasks, model, thinking, editor
        // (both spellings), stash, newline (both), recall — plus Ctrl+V,
        // the copy pair, retry (F5 + Alt+R) and Alt+Shift+T.
        let prompt: Vec<(String, ActionId)> = map
            .prompt_events()
            .iter()
            .map(|(event, id)| (format!("{event:?}"), *id))
            .collect();
        assert!(
            prompt
                .iter()
                .any(|(event, id)| event.contains("BackTab") && id == &ActionId::ModeCycle)
        );
        // Ctrl+G folds to the lowercase byte the terminal sends.
        assert!(prompt.iter().any(|(event, id)| event.contains("Char('g')")
            && event.contains("CTRL")
            && id == &ActionId::EditorExternal));
        assert!(
            prompt
                .iter()
                .any(|(event, id)| event.contains("F(5)") && id == &ActionId::Retry)
        );
        assert!(
            prompt
                .iter()
                .any(|(event, id)| event.contains("Char('v')") && id == &ActionId::PasteImage)
        );
        // Turn: CSI Z, Ctrl+O, Ctrl+S, Ctrl+X Ctrl+K, CSI-u Ctrl+Enter, Alt+Shift+T.
        let turn = map.turn_patterns();
        for (want, id) in [
            (b"\x1b[Z".to_vec(), ActionId::ModeCycle),
            (vec![0x0f], ActionId::ToolsExpand),
            (vec![0x13], ActionId::QueueSendNow),
            (vec![0x18, 0x0b], ActionId::StopAll),
            (b"\x1b[13;5u".to_vec(), ActionId::QueueSendNow),
            (vec![0x1b, b'T'], ActionId::ThinkingCycle),
        ] {
            assert!(
                turn.iter()
                    .any(|(pattern, got)| *pattern == want && got == &id),
                "{want:?} -> {id:?}"
            );
        }
        // Longest first, so the Ctrl+X pairs win their prefix.
        let lens: Vec<usize> = turn.iter().map(|(pattern, _)| pattern.len()).collect();
        assert!(lens.windows(2).all(|pair| pair[0] >= pair[1]));
    }

    #[test]
    fn undecodable_chords_warn_instead_of_failing() {
        let mut map = Keymap::default();
        map.overlay(
            "app.retry = \"Ctrl+Enter\"\napp.queue.sendNow = \"F7\"",
            Path::new("/tmp/k.toml"),
        )
        .unwrap();
        map.finish().unwrap();
        let warnings = map.warnings().join("\n");
        assert!(warnings.contains("app.retry"), "prompt layer: {warnings}");
        assert!(
            warnings.contains("app.queue.sendNow"),
            "turn layer: {warnings}"
        );
        // The action still exists, just with no way to fire there.
        assert!(
            map.prompt_events()
                .iter()
                .all(|(_, id)| id != &ActionId::Retry)
        );
    }

    #[test]
    fn disabling_an_action_removes_its_patterns() {
        let map = loaded("app.tools.expand = []\napp.mode.cycle = []\n");
        assert!(
            map.turn_patterns()
                .iter()
                .all(|(pattern, _)| pattern != &b"\x1b[Z".to_vec())
        );
        assert!(
            map.prompt_events()
                .iter()
                .all(|(_, id)| id != &ActionId::ModeCycle)
        );
    }

    #[test]
    fn load_from_is_defaults_when_missing_and_loud_when_malformed() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("keybindings.toml");
        // No file: the normal first run.
        assert!(matches!(load_from(&path), Ok(None)));
        // A typo'd action id names the file and the valid ids.
        std::fs::write(&path, "app.tools.expnad = \"Ctrl+O\"\n").unwrap();
        let err = load_from(&path).unwrap_err();
        assert!(
            err.contains("app.tools.expnad") && err.contains("app.tools.expand"),
            "{err}"
        );
        assert!(err.contains(&path.display().to_string()), "{err}");
        // A conflict found in finish() carries the path too.
        std::fs::write(&path, "app.thinking.cycle = \"Ctrl+O\"\n").unwrap();
        let err = load_from(&path).unwrap_err();
        assert!(
            err.contains("both bound") && err.contains(&path.display().to_string()),
            "{err}"
        );
        // A good file: remapped, source recorded, no warnings.
        std::fs::write(&path, "app.tasks.toggle = \"Ctrl+Y\"\n").unwrap();
        let map = load_from(&path).unwrap().expect("a file loads");
        assert!(map.is_remapped(ActionId::TasksToggle));
        assert_eq!(map.first_label(ActionId::TasksToggle), "Ctrl+Y");
        assert_eq!(map.source_path(), Some(path.as_path()));
        assert!(map.warnings().is_empty());
    }
}
