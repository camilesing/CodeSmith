#!/usr/bin/env node

const notice = [
  "",
  "  ╭───────────────────────────────────────────────────────────────────╮",
  "  │                                                                   │",
  "  │  deepseek-tui has been renamed to `codesmith`.                    │",
  "  │                                                                   │",
  "  │  Please uninstall this package and install codesmith instead:     │",
  "  │                                                                   │",
  "  │    npm uninstall -g deepseek-tui                                  │",
  "  │    npm install -g codesmith                                       │",
  "  │                                                                   │",
  "  │  codesmith ships the same `codesmith` and `codesmith-tui`         │",
  "  │  binaries plus deprecation shims under the old names. See:        │",
  "  │  https://github.com/Hmbown/CodeSmith/blob/main/docs/REBRAND.md │",
  "  │                                                                   │",
  "  ╰───────────────────────────────────────────────────────────────────╯",
  "",
].join("\n");

process.stderr.write(notice);
