# Keybindings

This is the source-of-truth catalog of every keyboard shortcut the TUI recognizes. Bindings are grouped by **context** — the focus or modal state they fire in. A binding listed under "Composer" only takes effect when the composer is focused; one under "Transcript" only when the transcript has focus; and so on.

Bindings are not (yet) user-configurable — planned for a future release. This document is the contract that future config-file overrides will name into.

## Global (any context)

| Chord                | Action                                                        |
|----------------------|---------------------------------------------------------------|
| `F1` or `Ctrl-/`     | Toggle the help overlay                                       |
| `Alt-?`              | Open the help overlay (when the composer is empty)             |
| `Ctrl-K`             | Open the command palette (slash-command finder)                |
| `Ctrl-C`             | Cancel current turn / dismiss modal / arm-then-confirm quit    |
| `Ctrl-D`             | Quit (only when the composer is empty)                         |
| `Tab`                | Cycle TUI mode: Plan → Agent → YOLO → Plan                     |
| `Shift-Tab`          | Cycle reasoning effort: off → high → max → off                 |
| `Ctrl-R`             | Open the resume-session picker                                 |
| `Ctrl-L`             | Force a full screen redraw (recovers garbled output)           |
| `Alt-M`              | Open the model picker (switches hot while a turn runs)         |
| `Ctrl-O`             | Open Activity Detail for selected/live/recent tool work, or the full reasoning timeline for thinking blocks when the composer is empty |
| `Ctrl-Shift-E` / `Cmd-Shift-E` | Toggle the file-tree sidebar                          |
| `Alt-G`              | Scroll transcript to top when the composer is empty             |
| `Alt-!` / `Alt-@` / `Alt-#` / `Alt-$` / `Alt-0` | Focus Work / Tasks / Agents / Context / Auto sidebar |
| `Ctrl-Alt-0`         | Hide the right sidebar                                          |
| `Esc`                | Close topmost modal · cancel slash menu · dismiss toast        |

## Composer

Editing the message you're about to send.

| Chord                       | Action                                                  |
|-----------------------------|---------------------------------------------------------|
| `Enter`                     | Send the message (or run the slash command)             |
| `Ctrl-Enter`                | Force-steer the draft into the running turn; on an empty composer, send all queued messages immediately |
| `Alt-Enter` / `Ctrl-J` / `Shift-Enter` | Insert a newline without sending              |
| `\` + `Enter`               | Line continuation: removes the trailing `\` and continues on a newline (`\\` keeps the backslash literal) |
| `Ctrl-U`                    | Kill to start of line (into the kill ring)               |
| `Ctrl-W`                    | Kill previous word (into the kill ring)                  |
| `Alt-D`                     | Kill to end of word (into the kill ring)                 |
| `Ctrl-A` / `Home`           | Move to start of line                                   |
| `Ctrl-E` / `End`            | Move to end of line                                     |
| `Ctrl-←` / `Alt-←`          | Move backward one word                                  |
| `Ctrl-→` / `Alt-→`          | Move forward one word                                   |
| `Ctrl-V` / `Cmd-V`          | Paste from clipboard (also bracketed-paste auto-handled)|
| `Ctrl-Y`                    | Yank (paste) from the kill ring; `Alt-Y` right after cycles earlier kills (yank-pop), otherwise `Alt-Y` toggles YOLO |
| `Ctrl-Z` / `Ctrl-_`         | Undo the last input edit (text + cursor; no redo)        |
| `↑` / `↓`                   | Cycle composer history (also selects popup/attachment items) |
| `Ctrl-P`                    | Open the fuzzy file picker                              |
| `Ctrl-S`                    | Stash current draft (`/stash list`, `/stash pop` to recover) |
| `Alt-R`                    | Search prompt history (Alt-R to exit)                  |
| `Tab`                       | Slash-command / `@`-mention / `:emoji:` completion (popup-aware) |
| `Ctrl-O`                    | Open external editor for the composer draft when it has focus |

### `@` mentions

Type `@<partial>` to open the file mention popup. `↑`/`↓` cycle the entries, `Tab` or `Enter` accepts. `Esc` hides the popup. Completions are re-ranked by mention frecency — files you mention often + recently float to the top.

### `#` quick-add (memory)

When `[memory] enabled = true`, typing `# foo` and pressing `Enter` appends `foo` as a timestamped bullet to your memory file *without* sending a turn. See `docs/MEMORY.md`.

### `!` shell passthrough

Type `!cmd` at the start of the input and press `Enter` to run `cmd` directly in your shell (`$SHELL`, falling back to `/bin/sh`; workspace as cwd). The command runs in the background — the UI keeps responding while it executes, and `Esc` cancels it. stdout and stderr are captured (60s timeout, output truncated at 16k chars) and submitted to the session as a user message so the model can respond to the output. The command runs as your own action and does not pass the tool-approval gate, except that a command `command_safety` classifies as dangerous (e.g. `curl … | sh`) gets an explicit confirmation prompt before it runs.

### `:` emoji shortcodes

Type `:name:` to insert an emoji — the closing `:` replaces the whole token (`:fire:` becomes 🔥). Typing two or more characters of a partial `:na…` opens a suggestion popup; `↑`/`↓` cycle, `Tab` or `Enter` accepts, `Esc` dismisses. The opening `:` only triggers at line start or after whitespace, so URLs and times like `12:30` are never touched. Unknown shortcodes stay as plain text.

## Transcript (when transcript has focus)

| Chord                | Action                                              |
|----------------------|-----------------------------------------------------|
| `↑` / `↓` / `j` / `k`| Scroll one line (bare arrows also scroll when the composer is empty) |
| `PgUp` / `PgDn`      | Scroll one page                                    |
| `Home` / `g`         | Jump to top                                         |
| `End` / `G`          | Jump to bottom                                     |
| `Esc`                | Return focus to composer                           |
| `y`                  | Yank selected region to clipboard                  |
| `v`                  | Begin / extend visual selection                    |
| `o`                  | Open URL under cursor (OSC 8 capable terminals)    |

## Sidebar (when sidebar has focus)

| Chord                | Action                                              |
|----------------------|-----------------------------------------------------|
| `↑` / `↓` / `j` / `k`| Move selection                                     |
| `Enter`              | Activate the selected item (open / focus / cancel) |
| `Tab`                | Cycle to next sidebar panel (Work → Tasks → Agents → Context) |
| `Esc`                | Return focus to composer                           |

## Slash-command palette (after `Ctrl-K` or typing `/`)

| Chord                | Action                                              |
|----------------------|-----------------------------------------------------|
| `↑` / `↓`            | Move selection                                     |
| `Enter` / `Tab`      | Run / complete the highlighted command             |
| `Esc`                | Dismiss palette                                     |

## Session Picker (`Ctrl-R` or `/sessions`)

| Chord                | Action                                              |
|----------------------|-----------------------------------------------------|
| `↑` / `↓` / `j` / `k`| Move selection in the session list                 |
| `1`-`9`              | Open the visible session history at that list slot |
| `PgUp` / `PgDn`      | Page the history pane                              |
| `Enter`              | Resume the selected session                        |
| `/`                  | Search sessions                                    |
| `s`                  | Cycle sort order                                   |
| `a`                  | Toggle current-workspace scope vs all workspaces   |
| `d`                  | Delete selected session after confirmation         |
| `Esc` / `q`          | Close the picker                                   |

## Approval modal (when a tool requests approval)

| Chord                | Action                                              |
|----------------------|-----------------------------------------------------|
| `y` / `Y`            | Approve once                                        |
| `a` / `A`            | Approve all (auto-approve subsequent calls)        |
| `n` / `N` / `Esc`    | Deny                                                |
| `e`                  | Edit the approved input before running              |

## Onboarding (first-run flow)

| Chord                | Action                                              |
|----------------------|-----------------------------------------------------|
| `Enter`              | Advance to next step (Welcome → Language → API → …) |
| `Esc`                | Step back one screen                                |
| `1`–`5`              | Pick a language (Language step)                    |
| `y` / `Y`            | Trust the workspace (Trust step)                   |
| `n` / `N`            | Skip the trust prompt                              |

## Editing notes and known limitations

- **Kill ring.** `Ctrl-U`, `Ctrl-W`, `Alt-D`, and `Ctrl-K` (kill-to-end-of-line) all save their text into a 16-entry kill ring; consecutive kills merge into one entry. `Ctrl-Y` yanks; `Alt-Y` immediately after a yank cycles earlier entries (yank-pop). Outside the post-yank window `Alt-Y` keeps its Yolo-mode shortcut.
- **Composer undo.** `Ctrl-Z` / `Ctrl+_` restore the previous edit state including cursor position (128-deep, no redo).
- **`\` + Enter inserts a line continuation** and **`Ctrl-L` redraws** the screen.
- **`Alt-M` opens the model picker**; `Alt-P` toggles Plan mode.
- **Ctrl+Enter on an empty composer flushes the queue** — the running turn gets the queued messages steered in immediately instead of waiting for turn end.
- **`Alt-D` kills to end of word**; `delete_word_forward` is also reachable via `Alt/Ctrl+Delete`.
- **Bare Up/Down arrows scroll the transcript when the composer is empty** (the `should_scroll_with_arrows` gate). This matters in virtual terminals (Ghostty, Codex, Kitty-protocol), where Cmd+Up / Alt+Up shortcuts are unavailable.
- **Shift+Enter / Alt+Enter insert newlines in VSCode on Windows.** crossterm's `PushKeyboardEnhancementFlags` command unconditionally returns `Unsupported` on Windows (`is_ansi_code_supported() == false`), so the Kitty keyboard protocol escape is written directly (`\x1b[>1u` / `\x1b[<1u`), bypassing crossterm's capability gate. VSCode integrated terminal and Windows Terminal ≥1.17 both honour the Kitty keyboard protocol; terminals that do not understand the sequences silently discard them.
- **Ctrl-S is stash, not history search**; `Alt-R` is history search.
- **Configurable keymap and `tui.toml` remain deferred.** The `TuiPrefs` struct and loader exist in `settings.rs` but are not wired at startup. The named-binding registry that would let `~/.codesmith/tui.toml` override individual entries is still pending.
- Every other chord listed above resolves to a live handler in `crates/tui/src/tui/ui.rs` (key-event dispatch) or `crates/tui/src/tui/app.rs` (mode + state transitions).
