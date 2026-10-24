//! Bundled skills — the review and verification workflows CodeSmith ships
//! as built-in skills (`/code-review`, `/security-review`, `/simplify`,
//! `/run`, `/verify-app`), its `/batch` parallel-changes workflow, the
//! authoring/teaching pair `/run-skill-generator` and `/update-config`, and
//! the research and extension pair `/deep-research` and
//! `/plugin-authoring`.
//!
//! They are skills, not commands with logic: each one is a prompt the model
//! follows, so they live beside the discovered skills and merge into the same
//! registry ([`crate::interactive::commands::skills_cmds::discover`]). A
//! user's own skill of the same name wins — `merge_registered` skips a
//! name collision — so an installed `code-review` skill overrides the
//! bundled one rather than being shadowed by it.
//!
//! Known limitations (recorded beside the behaviour it owns):
//!
//! - These skills are reachable from the REPL (`/code-review`, `/skill
//!   code-review`, `/skills`). The model's own system-prompt skill list is
//!   built from the workspace and user skill directories
//!   (`render_available_skills_context_for_workspace`), which does not merge
//!   this set — a bundled skill is something a user invokes, not something
//!   the model picks up on its own.
//! - The bodies are prompt templates, exactly like Claude Code's bundled
//!   skills: they steer the model, they do not run a checker themselves.

use codesmith_agent_runtime::skills::{Skill, SkillSource};

const CODE_REVIEW: &str = r#"Review the code changes in this repository for correctness bugs.

Read the diff before judging it (`git diff`, `git diff --staged`, and the named
target if the request gives one), then read enough surrounding code to know
what the change is supposed to do. A changed line is not evidence of a bug on
its own.

Report, in order of severity:

1. **Correctness** — logic that does not do what the surrounding code
   promises: wrong condition, off-by-one, ignored error, wrong variable,
   a guard that no longer holds, a caller not updated for a changed contract.
2. **State and data** — migrations, persistence, cache invalidation, partial
   writes, anything that loses or corrupts data on the unhappy path.
3. **Concurrency** — races, unawaited work, locks held across awaits,
   cancellation leaving state half-updated.
4. **Trust boundaries** — validation skipped, untrusted input reaching a
   shell or a path, secrets in logs.

For each finding give: file:line, what breaks, the concrete input or sequence
that triggers it, and the smallest fix. Say explicitly when you found nothing
in a category. Do not restate the diff, do not list style preferences, and do
not propose refactors — that is what `/simplify` is for."#;

const SECURITY_REVIEW: &str = r#"Review the current changes for security vulnerabilities, as a security
reviewer with repository access.

Establish what changed (`git diff` against the merge base of the default
branch, or the named target), then examine the changed surface for:

- **Injection** — shell commands built from data, SQL built by concatenation,
  template or path injection, header injection.
- **Authentication and authorization** — a new route or code path that skips
  the check its neighbours do, a check that can be bypassed by an unusual
  caller, an operation reachable by the wrong role.
- **Data exposure** — secrets, tokens, or personal data in logs, error
  messages, URLs, or committed files; a file written without the permissions
  its contents require.
- **Untrusted input** — deserialization of attacker-controlled data, archive
  extraction, path traversal, SSRF, resource exhaustion via unbounded input.
- **Dependencies** — a new dependency or a widened version range on a
  security-sensitive path.

Report only findings you can support with the code in front of you: file:line,
the attack that exploits it, the impact, and the fix. Rank by exploitability.
State plainly when the changes introduce no vulnerability you can name — a
short honest report beats an invented one."#;

const SIMPLIFY: &str = r#"Review the code in question for simplification opportunities, and apply the
ones you are confident about.

Look for:

- **Reuse** — a helper that already exists for exactly this, or two new
  copies of the same logic that should be one.
- **Deletion** — code nothing calls, guards that cannot fire, parameters
  never passed, abstractions with a single caller that do not earn their
  indirection.
- **Directness** — a hand-rolled version of something the standard library
  or an installed dependency already does; three layers of wrapping around
  one call; a flag threaded through code that does not use it.
- **Right level** — logic that belongs next to its data rather than in a
  distant module; a concept that exists only to satisfy a test.

Apply the changes that reduce code without changing behaviour, then run the
tests that cover the touched code and report what ran. Leave anything that
changes behaviour, contracts, or public interfaces as a recommendation
instead of an edit — mention it once, with the reason."#;

const RUN: &str = r#"Build and run this project, then confirm the change works in the running
program rather than only in its unit tests.

1. Find how this project is built and started — a README, a Makefile, a
   package manifest, a compose file, a prior CI run — and use that, not a
   guess.
2. Build it. If it does not build, report the failure exactly and stop.
3. Run it the way a user of this project would, with a real invocation.
4. Exercise the behaviour that changed, and quote the observed output. If a
   UI, server, or CLI has a way to be driven non-interactively, use that.
5. Report: the command you ran, what you observed, and how it differs from
   the behaviour before the change. If a step needs something this machine
   does not have (a service, a device, credentials), say so and name what is
   missing instead of substituting a smoke test and calling it done."#;

const VERIFY_APP: &str = r#"Confirm the change in question actually does what it claims, by observing
the program rather than by re-reading the code.

- Restate the claim in one sentence, in terms of observable behaviour.
- Build and run the project as `/run` describes.
- Reproduce the scenario the claim is about, including the edge the change
  is most likely to have broken.
- Quote raw output as evidence: the command, the exit status, the relevant
  lines. Where the claim is negative ("no longer crashes", "no longer writes
  X"), show the check that would have caught the old behaviour.
- End with one verdict per claim: verified, not verified, or not verifiable
  here — and for the last two, exactly what blocked it. Do not report a
  partial observation as a verification."#;

const BATCH: &str = r#"Carry out a large, mechanical change across this codebase as parallel units
of work.

1. **Reconnaissance.** Establish the size and shape of the change before
   proposing anything: how many call sites, which directories, what the
   variant patterns are, whether a codemod or a scripted edit would do the
   whole thing (if it would, say so and skip the parallel path entirely).
2. **Plan.** Decompose the work into independent units — between 5 and 30,
   never more than the work supports. Each unit must name the files it owns
   and must not need another unit's edits to compile. Present the plan as a
   numbered list: unit, files, what changes, how it will be checked. Stop
   here and wait for approval.
3. **Execute.** After approval, open one background sub-agent per unit with
   `agent_open` (they run while this conversation continues), and give each
   a self-contained prompt: the unit's files, the exact change, the command
   that verifies it, and the instruction to run that command and report its
   output. Isolate units that touch overlapping files — `git worktree add`
   for a scratch tree, or run those units one at a time instead.
4. **Land and report.** Collect each unit's result (`agent_eval`), apply or
   keep its changes, run the project's real check on the merged tree, and
   report per unit: done, changed files, verification output, and anything
   that failed or was skipped.

Do not start a unit before the plan is approved, do not let two units edit
the same file at once, and do not report a unit as done without quoting the
command that verified it."#;

const RUN_SKILL_GENERATOR: &str = r#"Teach the `/run` and `/verify` workflows how this project
actually builds and starts, by writing the project skills they should read.

1. Find out for real: the build system, the entry point, the launch command,
   the port or device it uses, the migration/seed step, how to invoke it
   non-interactively, and how the project's own tests are run. Read the
   README, Makefile, package manifest, compose file, CI workflow, and any
   script directory — do not guess from the language.
2. Prove each step by running it, in a clean state where that is cheap
   (`cargo build`, `npm ci`, `docker compose up --build`). Quote the commands
   that worked and the output that shows they worked.
3. Write two skills next to this project's skills, named exactly `run` and
   `verify-app` so each overrides its bundled twin (a project skill only
   wins under the registry's exact-name, first-wins merge). Each is
   a `SKILL.md` with frontmatter (`name`, `description`) and a body that lists
   the exact commands, the environment they need, how to observe the running
   program, and the failure each step is likely to hit. Write them where the
   project keeps skills so the next session finds them, and say where you
   wrote them.
4. Report what you could not establish — a service that needs credentials, a
   device that is not attached — instead of inventing a step for it."#;

const UPDATE_CONFIG: &str = r#"Turn a plain-language settings request into the right file edit and apply it.

1. Locate the configuration that owns the setting. In CodeSmith that is
   `config.toml` (`~/.codesmith/config.toml` for the user, `.codesmith/` in a
   project) for settings, `~/.codesmith/mcp.json` for MCP servers,
   `.codesmith/skills/` for skills, and the hooks table for shell hooks.
   `docs/CONFIGURATION.md` in this repository names every key and its default;
   read it before writing a key you are not certain of.
2. Say which file and which key you are about to change, and what the value
   will be, before changing it.
3. Make the smallest edit that satisfies the request: set or change the key
   rather than restructuring the file, and preserve comments and unrelated
   settings exactly.
4. Validate: re-read the file and confirm it still parses as TOML, and name
   the running command that shows the change taking effect (`/config <key>`,
   `/permissions list`, `/mcp`, `/hooks`). A setting only the next session
   reads is worth saying out loud.
5. If the request needs something that is not a setting — a new provider, a
   credential, a feature flag — say so and name the command that does it
   instead of writing a key that does nothing."#;

const DEEP_RESEARCH: &str = r#"Answer a research question from sources, with the evidence attached.

1. Restate the question as the specific claims that need support, and name
   what would count as an answer.
2. Search widely first: several independent queries, including the terms a
   practitioner would use and the terms an opponent would use. Run the
   searches in parallel where the interface allows it.
3. Read the promising sources rather than their snippets, and follow the
   citations that carry the actual numbers.
4. Cross-check every load-bearing claim against at least two independent
   sources, and say so when only one exists. Prefer primary sources (specs,
   papers, filings, official documentation) over summaries of them.
5. Write the answer with inline citations (URL or file plus the exact passage
   you relied on), separate what is established from what is disputed, state
   your confidence and what would change it, and list what you could not
   find. If the question cannot be answered from available sources, say that
   plainly instead of padding."#;

const PLUGIN_AUTHORING: &str = r#"Write a mod for this project (CodeSmith's extension format), following the
project's own reference rather than inventing an API.

1. Read `docs/MODS.md` in the CodeSmith repository — that is the authority for
   the mod format, the Rhai API surface, the manifest fields, and the
   example mods. If this project is not CodeSmith, say so and stop: mods are a
   CodeSmith extension format.
2. Read the mod it should resemble (there is a template and at least one
   worked example next to the docs) and the host-side registration path that
   loads it, so the mod you write matches what the loader expects.
3. Write the mod: manifest fields first, then the smallest script that does
   what was asked. Prefer the documented host functions over shelling out.
4. Say what it needs from the user: mods load only after explicit consent, so
   name the command that activates it.
5. Verify it: the project's own tests for mods, or a manual run through the
   documented activation path, and quote the output."#;

/// The bundled skills table (`name`, `description`, body), in `/skills`
/// order — the single source of truth shared by `registered()` and the
/// tests below, so the registration and the test expectations cannot drift
/// apart.
const SKILLS: &[(&str, &str, &str)] = &[
    (
        "code-review",
        "Review the current changes for correctness bugs (the /code-review workflow)",
        CODE_REVIEW,
    ),
    (
        "security-review",
        "Review the current changes for security vulnerabilities",
        SECURITY_REVIEW,
    ),
    (
        "simplify",
        "Review the changed code for simplification and apply what is safe",
        SIMPLIFY,
    ),
    (
        "run",
        "Build and run this project to see the change working",
        RUN,
    ),
    (
        "verify-app",
        "Verify a claim about the code by running the program and observing it",
        VERIFY_APP,
    ),
    (
        "batch",
        "Plan a large change as parallel units and run one sub-agent per unit",
        BATCH,
    ),
    (
        "run-skill-generator",
        "Teach /run and /verify how this project builds, starts, and is observed",
        RUN_SKILL_GENERATOR,
    ),
    (
        "update-config",
        "Turn a settings request into the right config edit and apply it",
        UPDATE_CONFIG,
    ),
    (
        "deep-research",
        "Answer a research question from cross-checked sources, with citations",
        DEEP_RESEARCH,
    ),
    (
        "plugin-authoring",
        // Bridge both vocabularies: the skill is invoked as
        // `plugin-authoring`, but the format it teaches is a "mod"
        // everywhere else (docs/MODS.md, `SkillSource::Extension`).
        "Write a mod (plugin) for CodeSmith, following docs/MODS.md",
        PLUGIN_AUTHORING,
    ),
];

/// The bundled skills, in `/skills` order.
#[must_use]
pub fn registered() -> Vec<Skill> {
    SKILLS
        .iter()
        .map(|&(name, description, body)| {
            let mut skill =
                Skill::with_defaults(name.to_string(), description.to_string(), body.to_string());
            skill.loaded_from = SkillSource::Bundled;
            skill
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bundled_skills_are_named_and_non_empty() {
        let skills = registered();
        // Compare against the table itself — a magic `10` here would bless
        // whatever set `registered()` happens to build.
        assert_eq!(skills.len(), SKILLS.len());
        for (skill, &(name, description, _body)) in skills.iter().zip(SKILLS) {
            assert_eq!(skill.name, name);
            assert_eq!(skill.description, description);
            assert!(
                !skill.description.is_empty(),
                "{name} has no description for the /skills list"
            );
            assert!(
                skill.body.len() > 200,
                "{name} body is too thin to steer a review"
            );
            assert_eq!(skill.loaded_from, SkillSource::Bundled);
            assert!(skill.user_invocable, "{name} must be invocable");
        }
        let names: Vec<&str> = skills.iter().map(|s| s.name.as_str()).collect();
        for &(name, _, _) in SKILLS {
            assert!(names.contains(&name), "{name} missing from registered()");
        }
    }
}
