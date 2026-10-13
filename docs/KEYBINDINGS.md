# Keyboard and interaction reference

Everything the REPL binds at the prompt and while a turn is running. `/help`
lists commands; this file lists keys. `?` on an empty prompt prints the same
table in the terminal, and `/keybindings` reprints it.

Two layers are in play:

- **At the prompt**, the line editor is [`rustyline`](https://github.com/kkawakam/rustyline)
  with its Emacs keymap (or its vi keymap — `/config editor_mode vim`). Most
  of the editing and history table below comes from it unchanged.
- **While a turn runs**, a small byte decoder owns stdin (only line
  buffering and echo are off; `Ctrl+C` still arrives as an interrupt) and
  feeds a composer: you can type, edit, and queue while the model works.

## While a turn is running (the composer)

| Key | Effect |
| :- | :- |
| *any text* | Types into the composer; the row appears under the status line |
| `Enter` | Queues the message — it is sent when the tool call it waited on finishes, or as the next turn if the turn ends first |
| `Ctrl+J` | Newline inside the composer (`Enter` queues, so this is how to write two lines) |
| `Ctrl+X Ctrl+S` | Sends everything queued (and the composer) to the running turn right away |
| `Ctrl+Enter` | The same *send now*, on terminals that report the modifier with Enter (kitty/CSI-u); `Ctrl+X Ctrl+S` works everywhere |
| `Esc` | Backgrounds the running shell or sub-agent if there is one and sends what you queued; otherwise interrupts the turn, and the queue goes out next |
| `Ctrl+B` | Sends the running shell to the background |
| `Ctrl+X Ctrl+K` | Stops all background work (press twice within 3 seconds) |
| `Ctrl+O` | Toggles collapsed/expanded tool-call detail |
| `Ctrl+T` | Toggles the model's task checklist |
| `Shift+Tab` | Cycles the run mode (applies from the next turn) |
| `Up` | With an empty composer, takes back the newest queued message for editing |
| `Down · Tab · completion` | Not available mid-turn (no history, no completion) |
| `Ctrl+C` | Interrupts the turn; press twice to quit |
| `Ctrl+A/E`, `Ctrl+K/U/W`, `←/→`, `Home/End`, `Backspace`, `Delete` | Editing inside the composer (kill keys have no kill ring mid-turn) |

Anything still in the composer when the turn ends comes back as the next
prompt's text — a turn that finishes first never swallows what you were
typing.

## At the prompt

### General

| Key | Effect |
| :- | :- |
| `Enter` | Submit |
| `Ctrl+C` | Clears the input (the draft is kept — `Up` recalls it); a second press within a second quits |
| `Ctrl+D` | With text: deletes the character after the cursor. On an empty line: a second press within 800 ms quits |
| `Ctrl+S` | Stashes the draft. Press again on an empty prompt to restore it |
| `Ctrl+O` | Toggles collapsed/expanded tool-call detail for the last turn |
| `Ctrl+T` | Toggles the task checklist panel above the prompt |
| `Ctrl+G` / `Ctrl+X Ctrl+E` | Opens the draft in `$VISUAL`/`$EDITOR` and takes back what you save |
| `Ctrl+X Ctrl+K` | Stops all background work (press twice within 3 seconds) |
| `Alt+P` (`⌥P`) | Toggles between the session model and the configured default model — `/model <id>` picks any |
| `Alt+T` (`⌥T`) | Toggles extended thinking (off ↔ high) |
| `Shift+Tab` | Cycles the run mode: `agent → plan → yolo` (plus `coordinator` when its feature is enabled) |
| `Esc` | In vi insert mode: leaves insert mode |
| `Esc`, `Esc` | With text: clears the draft and keeps it for `Up`. On an empty prompt: no-op (rewind's menu is not built; files are `/rewind`, the exchange is `/undo`) |
| `?` | On an empty prompt: prints the shortcut panel |
| `Ctrl+L` | Redraws the screen (rustyline) |
| `Ctrl+Z` | Suspends to the shell; `fg` returns (rustyline) |
| `Ctrl+R` | Reverse history search (rustyline) |

`Alt`/`Option` keys (`Alt+P`, `Alt+T`, `Alt+B/F/D`, `Alt+Y`) need the
terminal to send Option as Meta; on macOS that is a terminal setting — see
`/terminal-setup`.

### Editing (readline conventions, from rustyline)

| Key | Effect |
| :- | :- |
| `Ctrl+A` / `Ctrl+E` | Start / end of line |
| `Ctrl+K` / `Ctrl+U` | Kill to end / to start of line |
| `Ctrl+W` | Kill back to the previous whitespace — one press removes a whole path |
| `Backspace` / `Ctrl+H` | Delete the character before the cursor |
| `Ctrl+Y` / `Alt+Y` | Paste the last killed text / cycle the kill ring |
| `Alt+B` / `Alt+F` | Back / forward one word |
| `Alt+D` | Kill to the end of the word |
| `Ctrl+_` | Undo the last edit |
| `Ctrl+T` (emacs transpose) | Shadowed: `Ctrl+T` is the task checklist |

### Multiline input

| Method | Works in |
| :- | :- |
| A line ending in `\` then `Enter` | Every terminal. The `\` is a marker and is removed before the message is sent |
| `Ctrl+J` | Every terminal |
| `Shift+Enter` / `Alt+Enter` | Only terminals that report the modifier with Enter |
| Pasting multi-line text | Every terminal (bracketed paste); the newlines survive |

### History

| Key | Effect |
| :- | :- |
| `Up` / `Down`, `Ctrl+P` / `Ctrl+N` | Walk the history; `Up` on an empty prompt recalls a cleared draft first, then queued messages, then history |
| `Ctrl+R` | Reverse search; `Enter` accepts and sends, `Tab`/`Esc` accept and keep editing, `Ctrl+C` cancels |

History is stored **per workspace** in `~/.codesmith/history/<workspace>`,
capped by the `max_input_history` setting (default 500). The pre-upgrade
single-file history at `~/.codesmith/history` is read once for a workspace
that has no file of its own, and is left in place.

### Completion

| Key | Effect |
| :- | :- |
| `Tab` | Completes the word under the cursor; a second `Tab` lists the candidates |
| `Tab` on `/` | Command names (at the start of the prompt **and** mid-prompt, after a space) |
| `Tab` on `@` | Workspace paths, from a cached index (`.gitignore` respected, hidden files skipped, refreshed at most every 10 s, bounded by `mention_walk_depth` and `mention_menu_limit`) |
| `Tab` on an argument | The closed vocabulary the command declares (`/mode agent…`, `/effort high…`, `/mcp list…`) |
| `Tab` on a `!` line | A word containing `/` completes workspace paths; otherwise the project's own `!` history (newest first, duplicates collapsed) |

### `!` shell mode

A line starting with `!` runs in your own shell, in the workspace, prints its
output, and hands the command and output to the model as one message so the
turn answers from it. It needs no approval: the approval surface gates what
the model runs, not what you type. Set `respond_to_shell_commands = false` to
record the run without starting a turn.

Known limits: output is captured and printed when the command exits (not
streamed), both streams are merged, and the tail is capped at 20 000
characters.

## Vim mode

`/config editor_mode vim` (or `composer_vim_mode = "vim"` in settings.toml)
switches the prompt to rustyline's vi keymap: insert/command modes, motions
(`h j k l w b e 0 $ ^ f F t T ; ,`), operators (`d c y` with motions and
doubled forms), text objects (`iw aw i" a" i( a[ a{`), `x r s S ~`, `p P`,
`>> <<`, `J`, `u`, `.`, and `v`/`V` visual modes. `/config editor_mode
normal` returns to Emacs keys, and the switch applies at the next prompt.

Not implemented, with the reason: `vimInsertModeRemaps` (Claude Code's `jj` →
`Escape`). rustyline 14 exposes no command a key handler could use to leave
vi insert mode, so a handler cannot synthesize the `Escape`.

## What the terminal has to do

| Feature | Needs |
| :- | :- |
| `Shift+Tab` | The terminal must send `ESC [ Z` (Terminal.app, iTerm2, tmux defaults do); the kitty protocol's `CSI 1;2Z` is not decoded by rustyline 14 |
| `Alt`/`Option` keys | Option-as-Meta in the terminal (`/terminal-setup` prints the setting for each) |
| `Ctrl+Enter` | A terminal that reports the modifier with Enter; `Ctrl+X Ctrl+S` is the portable spelling |
| `Shift+Enter` | Same as above; `Ctrl+J` and trailing `\` work everywhere |
| Composer, mid-turn keys | A real TTY. Piped input (`codesmith < file`) leaves the mid-turn tap inert by design |
| `/copy`, image paste | A clipboard tool on `PATH` (`pbcopy`, `wl-copy`, `xclip`, `clip`); the command names what to install when none is found |

## Not bound here

- Block-wise visual mode (`Ctrl+V` in Claude Code's vim mode).
- A keybindings configuration file: the bindings are compiled in, and
  `/keybindings` is a read-only table (`/config editor_mode` is the only
  keyboard setting).
- Fullscreen-only interactions (transcript viewer keys such as `{`/`}`,
  mouse support, `/scroll-speed`, `/focus`).
- Voice dictation.