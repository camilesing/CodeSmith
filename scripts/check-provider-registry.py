#!/usr/bin/env python3
"""Check that docs/PROVIDERS.md tracks the shipped provider registry.

This is intentionally lightweight. It does not try to generate prose; it checks
the stable identifiers and default strings that are easy for docs to drift from:

- canonical ProviderKind IDs
- provider TOML tables
- live ApiProvider IDs
- shipped-provider table rows
- static ModelRegistry provider rows
- default provider model/base URL constants
"""

from __future__ import annotations

import re
import sys
from pathlib import Path


ROOT = Path(__file__).resolve().parents[1]
CONFIG_RS = ROOT / "crates" / "config" / "src" / "lib.rs"
TUI_CONFIG_RS = ROOT / "crates" / "cli" / "src" / "config.rs"
# The ApiProvider enum/impl lives in agent-runtime; the CLI's config.rs
# re-exports it.
AGENT_RUNTIME_CONFIG_RS = ROOT / "crates" / "agent-runtime" / "src" / "config_types.rs"
AGENT_RS = ROOT / "crates" / "agent" / "src" / "lib.rs"
PROVIDERS_MD = ROOT / "docs" / "PROVIDERS.md"


# ApiProvider IDs that legitimately exist outside ProviderKind. Empty since the
# legacy `deepseek-cn` variant was folded onto `deepseek` (§B3 slice 52); the
# `deepseek-cn` spellings survive only as parse aliases, not as an enum variant.
API_PROVIDER_ONLY_IDS: set[str] = set()


def read(path: Path) -> str:
    return path.read_text(encoding="utf-8")


def require_index(source: str, needle: str, context: str, start: int = 0) -> int:
    try:
        return source.index(needle, start)
    except ValueError:
        raise ValueError(f"{context}: missing {needle!r}") from None


def markdown_section(source: str, heading: str) -> str:
    start = require_index(source, heading, "docs/PROVIDERS.md")
    # Terminate at ANY ATX heading: stopping only at "\n## " pulled `###`
    # subsection content (e.g. "### Xiaomi MiMo Notes") into the section, so
    # tables added to a subsection would leak into the row sets.
    next_heading = re.search(r"\n#{1,6} ", source[start + len(heading) :])
    end = (
        len(source) if next_heading is None else start + len(heading) + next_heading.start()
    )
    return source[start:end]


def _skip_string_literal(source: str, index: int) -> int:
    # index points at the opening quote; return the index just past the
    # closing quote. Raw strings (r"…") are not special-cased — the as_str
    # blocks under scan use plain literals.
    quote = source[index]
    index += 1
    while index < len(source):
        if source[index] == "\\":
            index += 2
            continue
        if source[index] == quote:
            return index + 1
        index += 1
    return index


def extract_match_block(
    source: str, signature: str, context: str, start: int = 0
) -> str:
    start = require_index(source, signature, context, start)
    match_start = require_index(
        source, "match self {", f"match block after {signature!r}", start
    )
    brace_start = match_start + len("match self {") - 1
    depth = 0
    index = brace_start
    while index < len(source):
        char = source[index]
        # Braces inside string/char literals and comments would otherwise
        # truncate or extend the extracted block and silently yield a wrong
        # provider-ID set.
        if char in "\"'":
            index = _skip_string_literal(source, index)
            continue
        if source.startswith("//", index):
            newline = source.find("\n", index)
            index = len(source) if newline == -1 else newline
            continue
        if char == "{":
            depth += 1
        elif char == "}":
            depth -= 1
            if depth == 0:
                return source[brace_start + 1 : index]
        index += 1
    raise ValueError(f"could not parse match block after {signature!r}")


def as_str_pairs(block: str, enum_name: str) -> dict[str, str]:
    pairs = re.findall(r"Self::(\w+)\s*=>\s*\"([^\"]+)\"", block)
    if not pairs:
        raise ValueError(f"{enum_name}::as_str returned no providers")
    # Or-patterns (`Self::A | Self::B => "x"`), guards, and block bodies are
    # invisible to the arm regex — a silent partial parse is a false pass on
    # exactly the drift this script exists to catch, so demand that every
    # `Self::` occurrence was accounted for.
    if len(re.findall(r"Self::", block)) != len(pairs):
        raise ValueError(
            f"{enum_name}::as_str contains arms the drift check cannot "
            "parse (or-patterns, guards, or block bodies)"
        )
    return {variant: provider_id for variant, provider_id in pairs}


def provider_kind_ids(config_rs: str) -> dict[str, str]:
    impl_start = require_index(
        config_rs, "impl ProviderKind", "crates/config/src/lib.rs"
    )
    block = extract_match_block(
        config_rs,
        "pub fn as_str(self) -> &'static str",
        "crates/config/src/lib.rs",
        impl_start,
    )
    return as_str_pairs(block, "ProviderKind")


def api_provider_ids(config_types_rs: str) -> dict[str, str]:
    impl_start = require_index(
        config_types_rs,
        "impl ApiProvider",
        "crates/agent-runtime/src/config_types.rs",
    )
    block = extract_match_block(
        config_types_rs,
        "pub fn as_str(self) -> &'static str",
        "crates/agent-runtime/src/config_types.rs",
        impl_start,
    )
    return as_str_pairs(block, "ApiProvider")


def provider_tables(config_rs: str) -> set[str]:
    struct_start = require_index(
        config_rs, "pub struct ProvidersToml", "crates/config/src/lib.rs"
    )
    struct_end = require_index(config_rs, "\n}", "ProvidersToml struct", struct_start)
    fields = re.findall(
        r"pub\s+([a-z0-9_]+)\s*:\s*ProviderConfigToml",
        config_rs[struct_start:struct_end],
    )
    if not fields:
        raise ValueError("ProvidersToml returned no provider tables")
    return set(fields)


def shipped_provider_rows(providers_md: str) -> set[str]:
    table = markdown_section(providers_md, "## Shipped Providers")
    return set(re.findall(r"^\|\s*`([^`]+)`\s*\|", table, flags=re.MULTILINE))


def shipped_provider_tables(providers_md: str) -> set[str]:
    table = markdown_section(providers_md, "## Shipped Providers")
    return set(re.findall(r"\|\s*`\[providers\.([a-z0-9_]+)\]`\s*\|", table))


def static_registry_provider_rows(providers_md: str) -> set[str]:
    table = markdown_section(providers_md, "## Static Model Registry")
    return set(re.findall(r"^\|\s*`([^`]+)`\s*\|", table, flags=re.MULTILINE))


def model_registry_providers(agent_rs: str, variant_to_id: dict[str, str]) -> set[str]:
    variants = set(re.findall(r"provider:\s*ProviderKind::(\w+)", agent_rs))
    missing = variants - set(variant_to_id)
    if missing:
        raise ValueError(f"ModelRegistry uses unknown provider variants: {sorted(missing)}")
    return {variant_to_id[variant] for variant in variants}


def default_strings(cli_config_rs: str) -> set[str]:
    defaults = set()
    for name, value in re.findall(
        r'const\s+(DEFAULT_[A-Z0-9_]+(?:MODEL|BASE_URL)):\s*&str\s*=\s*"([^"]+)"',
        cli_config_rs,
    ):
        defaults.add(value)
    if not defaults:
        raise ValueError("no default provider model/base URL constants found")
    return defaults


def missing_default_strings(providers_md: str, defaults: set[str]) -> list[str]:
    # Inline-code validation should not let fenced TOML/bash examples pair a
    # stray backtick with later prose; strip fenced blocks before scanning.
    # Tilde fences are stripped too, and code spans cannot cross lines — an
    # accidentally created code span must not mask an undocumented default.
    inline_source = re.sub(r"(```|~~~)[\s\S]*?\1", "", providers_md)
    code_spans = set(re.findall(r"`([^`\n]+)`", inline_source))
    return sorted(defaults - code_spans)


def report_set(label: str, expected: set[str], actual: set[str]) -> list[str]:
    errors = []
    missing = sorted(expected - actual)
    extra = sorted(actual - expected)
    if missing:
        errors.append(f"{label} missing: {', '.join(missing)}")
    if extra:
        errors.append(f"{label} extra: {', '.join(extra)}")
    return errors


def report_provider_enum_drift(
    canonical_ids: set[str], live_api_ids: set[str]
) -> list[str]:
    errors = []
    missing_from_api_provider = sorted(canonical_ids - live_api_ids)
    unexpected_api_provider_ids = sorted(
        live_api_ids - canonical_ids - API_PROVIDER_ONLY_IDS
    )
    missing_allowlisted_ids = sorted(API_PROVIDER_ONLY_IDS - live_api_ids)

    if missing_from_api_provider:
        errors.append(
            "ApiProvider missing ProviderKind IDs: "
            + ", ".join(missing_from_api_provider)
        )
    if unexpected_api_provider_ids:
        errors.append(
            "ApiProvider has non-whitelisted IDs absent from ProviderKind: "
            + ", ".join(unexpected_api_provider_ids)
        )
    if missing_allowlisted_ids:
        errors.append(
            "ApiProvider-only whitelist entries are absent from ApiProvider: "
            + ", ".join(missing_allowlisted_ids)
        )
    return errors


def main() -> int:
    try:
        config_rs = read(CONFIG_RS)
        cli_config_rs = read(TUI_CONFIG_RS)
        agent_runtime_config_rs = read(AGENT_RUNTIME_CONFIG_RS)
        agent_rs = read(AGENT_RS)
        providers_md = read(PROVIDERS_MD)

        variant_to_id = provider_kind_ids(config_rs)
        canonical_ids = set(variant_to_id.values())
        live_api_provider_ids = set(api_provider_ids(agent_runtime_config_rs).values())
        expected_tables = {provider_id.replace("-", "_") for provider_id in canonical_ids}

        errors: list[str] = []
        errors += report_provider_enum_drift(canonical_ids, live_api_provider_ids)
        errors += report_set(
            "shipped provider rows",
            canonical_ids,
            shipped_provider_rows(providers_md),
        )
        errors += report_set("provider TOML tables", expected_tables, provider_tables(config_rs))
        errors += report_set(
            "documented provider TOML tables",
            expected_tables,
            shipped_provider_tables(providers_md),
        )
        errors += report_set(
            "static ModelRegistry rows",
            model_registry_providers(agent_rs, variant_to_id),
            static_registry_provider_rows(providers_md),
        )

        missing_defaults = missing_default_strings(providers_md, default_strings(cli_config_rs))
        if missing_defaults:
            errors.append(
                "docs/PROVIDERS.md does not mention default strings as Markdown code spans: "
                + ", ".join(missing_defaults)
            )
    except (ValueError, OSError) as err:
        errors = [str(err)]

    if errors:
        print("Provider registry drift check failed:", file=sys.stderr)
        for error in errors:
            print(f"- {error}", file=sys.stderr)
        return 1

    print("Provider registry drift check passed.")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
