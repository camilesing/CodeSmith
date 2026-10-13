//! Bundled skills — the review and verification workflows Claude Code ships
//! as built-in skills (`/code-review`, `/security-review`, `/simplify`,
//! `/run`, `/verify`).
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

/// The bundled skills, in `/skills` order.
#[must_use]
pub fn registered() -> Vec<Skill> {
    [
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
    ]
    .into_iter()
    .map(|(name, description, body)| {
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
        assert_eq!(skills.len(), 5);
        for skill in &skills {
            assert!(!skill.name.is_empty());
            assert!(
                !skill.description.is_empty(),
                "{} has no description for the /skills list",
                skill.name
            );
            assert!(
                skill.body.len() > 200,
                "{} body is too thin to steer a review",
                skill.name
            );
            assert_eq!(skill.loaded_from, SkillSource::Bundled);
            assert!(skill.user_invocable, "{} must be invocable", skill.name);
        }
        let names: Vec<&str> = skills.iter().map(|s| s.name.as_str()).collect();
        assert!(names.contains(&"code-review"));
        assert!(names.contains(&"verify-app"));
    }
}
