//! Bash arity dictionary for command-prefix allow rule matching.
//!
//! [`BashArityDict`] maps a command prefix (space-separated, lowercase) to the
//! number of positional (non-flag) words, *including the base command word*,
//! that form the canonical prefix.
//!
//! ## Invariant
//!
//! Flags (tokens starting with `-`) are **never** counted toward arity.
//! `auto_allow = ["git status"]` must match `git status -s` and
//! `git status --porcelain`, but **not** `git push`.
//!
//! Two hardening rules guard the match itself:
//!
//! 1. Flags whose value *executes* a command (`--receive-pack=`,
//!    `--upload-pack=`, `--exec=`, ssh `-oProxy*`, git `-c core.sshCommand=`)
//!    deny the match outright — an allow rule never authorizes them, whatever
//!    the base command.
//! 2. A bare single-word pattern whose base command has more specific arity
//!    entries (e.g. `"git"` with `git push` in the table) does not silently
//!    cover every subcommand; the rule must name the subcommand.
//!
//! ## Known limitations
//!
//! The flag deny-list is not exhaustive. Exec-by-positioning vectors
//! (`xargs <cmd>`, `ssh host cmd`, `docker run <img> <cmd>`,
//! `kubectl exec`, `go/cargo/bun run`, `npx <pkg>`, `python -m <module>`)
//! are denied outright by [`is_exec_by_positioning`] — an allow rule never
//! covers them; they fall back to the ordinary approval path.
//!
//! ## Coverage
//!
//! 30+ common tools are covered across: git, npm, yarn, pnpm, cargo, docker,
//! kubectl, go, python/pip, gh, rustup, deno, bun, aws, terraform, make,
//! and more.

/// Static arity table: `(prefix, arity)`.
///
/// Arity is the total number of *positional* tokens (including the base
/// command) that form the canonical prefix.  For example:
///
/// * `("git status", 2)` — 2 positional tokens: `git` + `status`.
/// * `("npm run", 3)` — 3 positional tokens: `npm` + `run` + `<script>`.
/// * `("make", 1)` — only the base command, no sub-command.
pub static BASH_ARITY_TABLE: &[(&str, u8)] = &[
    // ── git ──────────────────────────────────────────────────────────────────
    ("git add", 2),
    ("git am", 2),
    ("git apply", 2),
    ("git bisect", 2),
    ("git blame", 2),
    ("git branch", 2),
    ("git cat-file", 2),
    ("git checkout", 2),
    ("git cherry-pick", 2),
    ("git clean", 2),
    ("git clone", 2),
    ("git commit", 2),
    ("git config", 2),
    ("git describe", 2),
    ("git diff", 2),
    ("git fetch", 2),
    ("git format-patch", 2),
    ("git grep", 2),
    ("git init", 2),
    ("git log", 2),
    ("git ls-files", 2),
    ("git merge", 2),
    ("git mv", 2),
    ("git notes", 2),
    ("git pull", 2),
    ("git push", 2),
    ("git rebase", 2),
    ("git reflog", 2),
    ("git remote", 2),
    ("git reset", 2),
    ("git restore", 2),
    ("git revert", 2),
    ("git rm", 2),
    ("git show", 2),
    ("git stash", 2),
    ("git status", 2),
    ("git submodule", 2),
    ("git switch", 2),
    ("git tag", 2),
    ("git worktree", 2),
    // ── npm ──────────────────────────────────────────────────────────────────
    ("npm audit", 2),
    ("npm build", 2),
    ("npm cache", 2),
    ("npm ci", 2),
    ("npm dedupe", 2),
    ("npm fund", 2),
    ("npm help", 2),
    ("npm info", 2),
    ("npm init", 2),
    ("npm install", 2),
    ("npm link", 2),
    ("npm list", 2),
    ("npm ls", 2),
    ("npm outdated", 2),
    ("npm pack", 2),
    ("npm prune", 2),
    ("npm publish", 2),
    ("npm rebuild", 2),
    ("npm run", 3),
    ("npm start", 2),
    ("npm stop", 2),
    ("npm test", 2),
    ("npm uninstall", 2),
    ("npm update", 2),
    ("npm version", 2),
    ("npm view", 2),
    // ── yarn ─────────────────────────────────────────────────────────────────
    ("yarn add", 2),
    ("yarn audit", 2),
    ("yarn build", 2),
    ("yarn install", 2),
    ("yarn run", 3),
    ("yarn start", 2),
    ("yarn test", 2),
    ("yarn upgrade", 2),
    ("yarn workspace", 3),
    // ── pnpm ─────────────────────────────────────────────────────────────────
    ("pnpm add", 2),
    ("pnpm build", 2),
    ("pnpm install", 2),
    ("pnpm run", 3),
    ("pnpm start", 2),
    ("pnpm test", 2),
    ("pnpm update", 2),
    // ── cargo ────────────────────────────────────────────────────────────────
    ("cargo add", 2),
    ("cargo bench", 2),
    ("cargo build", 2),
    ("cargo check", 2),
    ("cargo clean", 2),
    ("cargo clippy", 2),
    ("cargo doc", 2),
    ("cargo fix", 2),
    ("cargo fmt", 2),
    ("cargo generate", 2),
    ("cargo install", 2),
    ("cargo metadata", 2),
    ("cargo package", 2),
    ("cargo publish", 2),
    ("cargo remove", 2),
    ("cargo run", 2),
    ("cargo search", 2),
    ("cargo test", 2),
    ("cargo tree", 2),
    ("cargo uninstall", 2),
    ("cargo update", 2),
    ("cargo yank", 2),
    // ── docker ───────────────────────────────────────────────────────────────
    ("docker build", 2),
    ("docker compose", 3),
    ("docker container", 3),
    ("docker cp", 2),
    ("docker exec", 2),
    ("docker image", 3),
    ("docker images", 2),
    ("docker inspect", 2),
    ("docker kill", 2),
    ("docker logs", 2),
    ("docker network", 3),
    ("docker ps", 2),
    ("docker pull", 2),
    ("docker push", 2),
    ("docker rm", 2),
    ("docker rmi", 2),
    ("docker run", 2),
    ("docker start", 2),
    ("docker stop", 2),
    ("docker system", 3),
    ("docker tag", 2),
    ("docker volume", 3),
    // ── kubectl ──────────────────────────────────────────────────────────────
    ("kubectl apply", 2),
    ("kubectl create", 3),
    ("kubectl delete", 3),
    ("kubectl describe", 3),
    ("kubectl exec", 2),
    ("kubectl explain", 2),
    ("kubectl get", 3),
    ("kubectl label", 2),
    ("kubectl logs", 2),
    ("kubectl patch", 2),
    ("kubectl port-forward", 2),
    ("kubectl rollout", 3),
    ("kubectl scale", 2),
    ("kubectl set", 2),
    ("kubectl top", 3),
    // ── go ───────────────────────────────────────────────────────────────────
    ("go build", 2),
    ("go clean", 2),
    ("go env", 2),
    ("go fmt", 2),
    ("go generate", 2),
    ("go get", 2),
    ("go install", 2),
    ("go list", 2),
    ("go mod", 3),
    ("go run", 2),
    ("go test", 2),
    ("go vet", 2),
    ("go work", 3),
    // ── python / pip ─────────────────────────────────────────────────────────
    ("pip install", 2),
    ("pip uninstall", 2),
    ("pip list", 2),
    ("pip show", 2),
    ("pip freeze", 2),
    ("pip3 install", 2),
    ("pip3 uninstall", 2),
    ("pip3 list", 2),
    ("pip3 show", 2),
    // NOTE: no "python -m"/"python3 -m" entries — `classify` strips flag
    // tokens before building candidates, so they were unreachable dead
    // weight, and a positional `<python> -m <module>` payload executes the
    // module: rules must name the module explicitly and route through
    // `is_exec_by_positioning` instead.
    // ── make / cmake ─────────────────────────────────────────────────────────
    ("make", 1),
    ("cmake", 1),
    // ── gh (GitHub CLI) ──────────────────────────────────────────────────────
    ("gh pr", 3),
    ("gh issue", 3),
    ("gh repo", 3),
    ("gh release", 3),
    ("gh workflow", 3),
    ("gh run", 3),
    ("gh secret", 3),
    // ── rustup ───────────────────────────────────────────────────────────────
    ("rustup default", 2),
    ("rustup install", 2),
    ("rustup show", 2),
    ("rustup target", 3),
    ("rustup toolchain", 3),
    ("rustup update", 2),
    // ── deno / bun ───────────────────────────────────────────────────────────
    ("deno run", 2),
    ("deno test", 2),
    ("deno fmt", 2),
    ("deno lint", 2),
    ("bun add", 2),
    ("bun build", 2),
    ("bun install", 2),
    ("bun run", 3),
    ("bun test", 2),
    ("npx", 2),
    // ── aws CLI ──────────────────────────────────────────────────────────────
    ("aws s3", 3),
    ("aws ec2", 3),
    ("aws iam", 3),
    ("aws lambda", 3),
    ("aws cloudformation", 3),
    ("aws ecs", 3),
    ("aws eks", 3),
    ("aws rds", 3),
    ("aws sts", 3),
    ("aws configure", 2),
    // ── terraform ────────────────────────────────────────────────────────────
    ("terraform init", 2),
    ("terraform plan", 2),
    ("terraform apply", 2),
    ("terraform destroy", 2),
    ("terraform validate", 2),
    ("terraform output", 2),
    ("terraform state", 3),
    ("terraform workspace", 3),
    // ── helm ─────────────────────────────────────────────────────────────────
    ("helm install", 2),
    ("helm upgrade", 2),
    ("helm uninstall", 2),
    ("helm list", 2),
    ("helm repo", 3),
    ("helm status", 2),
    ("helm template", 2),
];

/// Arity dictionary for bash command-prefix allow rules.
///
/// Provides arity-aware prefix extraction so that `auto_allow = ["git status"]`
/// correctly matches `git status -s` and `git status --porcelain` without
/// also matching `git push`.
///
/// # Example
///
/// ```rust
/// use codesmith_execpolicy::bash_arity::BashArityDict;
///
/// let dict = BashArityDict::new();
/// assert_eq!(dict.classify(&["git", "status", "-s"]),   "git status");
/// assert_eq!(dict.classify(&["git", "push", "origin"]), "git push");
/// assert_eq!(dict.classify(&["npm", "run", "dev"]),     "npm run dev");
/// assert_eq!(dict.classify(&["ls", "-la"]),             "ls");
/// ```
#[derive(Debug, Clone)]
pub struct BashArityDict {
    /// Internal table sorted longest-prefix-first for greedy matching.
    entries: Vec<(&'static str, u8)>,
}

impl BashArityDict {
    /// Construct a new dictionary pre-loaded with [`BASH_ARITY_TABLE`].
    #[must_use]
    pub fn new() -> Self {
        let mut entries: Vec<(&'static str, u8)> = BASH_ARITY_TABLE.to_vec();
        // Longest prefix first so greedy matching works correctly.
        entries.sort_by_key(|entry| std::cmp::Reverse(entry.0.len()));
        Self { entries }
    }

    /// Return the canonical command prefix for a slice of command tokens.
    ///
    /// # Algorithm
    ///
    /// 1. Strip all flag tokens (tokens that start with `-`).
    /// 2. Build candidates of depth 1..=3 from positional tokens (longest first).
    /// 3. If a candidate matches a dictionary entry, return `arity` positional
    ///    tokens joined with spaces.
    /// 4. If no dictionary entry matches, return the single base command name.
    #[must_use]
    pub fn classify(&self, tokens: &[&str]) -> String {
        if tokens.is_empty() {
            return String::new();
        }

        // Collect positional (non-flag) tokens, lowercased.
        let positional: Vec<String> = tokens
            .iter()
            .filter(|t| !t.starts_with('-'))
            .map(|t| t.to_ascii_lowercase())
            .collect();

        if positional.is_empty() {
            return String::new();
        }

        // Try candidates from longest to shortest (max depth 3).
        let max_depth = positional.len().min(3);
        for depth in (1..=max_depth).rev() {
            let candidate = positional[..depth].join(" ");
            if let Some(&(_key, arity)) = self
                .entries
                .iter()
                .find(|(key, _)| *key == candidate.as_str())
            {
                let take = (arity as usize).min(positional.len());
                return positional[..take].join(" ");
            }
        }

        // No match: return base command name only.
        positional[0].clone()
    }

    /// Return `true` if the allow-rule `pattern` (a command prefix string such
    /// as `"git status"`) matches the concrete command `command`.
    ///
    /// Matching is arity-aware:
    /// - `"git status"` matches `"git status -s"` and `"git status --porcelain"`.
    /// - `"git status"` does **not** match `"git push origin main"`.
    /// - Exact string patterns (e.g. `"ls"`) still work as before.
    ///
    /// Commands carrying an exec-capable flag never match (see the module
    /// invariant), and a bare base-command pattern whose command has more
    /// specific arity entries (e.g. `"git"`) does not cover those
    /// subcommands.
    ///
    /// For patterns that are not in the arity table, the function falls back to
    /// a plain prefix test on the normalised command so that existing exact-match
    /// rules continue to work unchanged.
    #[must_use]
    pub fn allow_rule_matches(&self, pattern: &str, command: &str) -> bool {
        let pattern_lower = pattern.trim().to_ascii_lowercase();
        let command_tokens: Vec<&str> = command.split_whitespace().collect();

        if contains_exec_capable_flag(&command_tokens) || is_exec_by_positioning(&command_tokens) {
            return false;
        }

        // Classify the concrete command through the arity dictionary.
        let canonical = self.classify(&command_tokens);

        // Primary check: the classified prefix equals the allow-rule pattern.
        if canonical == pattern_lower {
            return true;
        }

        // Fallback: plain normalised prefix match for patterns not in the table
        // (preserves backward compatibility with exact-match allow rules).
        let command_lower = command.trim().to_ascii_lowercase();
        // Normalise whitespace in both sides before comparing.
        let pattern_norm: String = pattern_lower
            .split_whitespace()
            .collect::<Vec<_>>()
            .join(" ");
        let command_norm: String = command_lower
            .split_whitespace()
            .collect::<Vec<_>>()
            .join(" ");

        // A single-word pattern whose base command has more specific arity
        // entries must not cover those subcommands: `auto_allow = ["git"]`
        // does not authorize `git push`.
        let Some(base_word) = pattern_norm.split(' ').next() else {
            return false;
        };
        if pattern_norm == base_word
            && self
                .entries
                .iter()
                .any(|(key, _)| key.starts_with(&format!("{base_word} ")))
        {
            return false;
        }

        command_norm == pattern_norm || command_norm.starts_with(&format!("{pattern_norm} "))
    }

    /// Iterate over all entries in the dictionary.
    pub fn entries(&self) -> impl Iterator<Item = (&str, u8)> {
        self.entries.iter().map(|(k, v)| (*k, *v))
    }

    /// Return the number of entries in the dictionary.
    #[must_use]
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// Return `true` if the dictionary is empty.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }
}

impl Default for BashArityDict {
    fn default() -> Self {
        Self::new()
    }
}

/// Flags whose value executes a command when the matched tool runs it
/// (`git push --receive-pack=<cmd>` runs `<cmd>` through the transport,
/// `ssh -oProxyCommand=<cmd>` runs it locally). An allow rule never
/// authorizes a command carrying one of these.
fn contains_exec_capable_flag(tokens: &[&str]) -> bool {
    // Exact names plus their flag families: `find -execdir` must die with
    // `-exec`, and `git --exec-path=<dir>` redirects git's subcommand
    // lookup — an exact-name list let both through.
    const EXEC_FLAG_NAMES: &[&str] = &["--receive-pack", "--upload-pack"];
    tokens.iter().enumerate().any(|(idx, token)| {
        let lower = token.to_ascii_lowercase();
        let flag_name = lower.split('=').next().unwrap_or("");
        if EXEC_FLAG_NAMES.contains(&flag_name)
            || flag_name == "--exec"
            || flag_name.starts_with("--exec-")
            || flag_name.starts_with("-exec")
        {
            return true;
        }
        // ssh `-oProxyCommand=…` (attached) or `-o ProxyCommand=…` (split).
        if lower.starts_with("-o") && lower[2..].starts_with("proxy") {
            return true;
        }
        if lower == "-o"
            && tokens
                .get(idx + 1)
                .is_some_and(|next| next.to_ascii_lowercase().starts_with("proxy"))
        {
            return true;
        }
        // git `-c core.sshCommand=<cmd>` and friends execute their value
        // (`credential.helper=!<cmd>`, `diff.external=<cmd>` included).
        is_exec_capable_git_key(&lower)
    }) || git_config_carries_exec_key(tokens) || has_executing_transport(tokens)
}

/// git configuration keys whose persisted value is executed by a later git
/// invocation (ssh command, editor, pager, credential helper, diff tool).
fn is_exec_capable_git_key(token: &str) -> bool {
    const EXEC_KEYS: &[&str] = &[
        "core.sshcommand=",
        "core.editor=",
        "core.pager=",
        "credential.helper=",
        "diff.external=",
        "core.fsmonitor=",
    ];
    EXEC_KEYS.iter().any(|key| token.contains(key))
}

/// `git config --global core.sshCommand /tmp/evil` — the split form carries
/// no `=`, so the attached-form `contains` checks miss it, yet the persisted
/// value is executed by the next auto-allowed `git fetch`/`git push`.
fn git_config_carries_exec_key(tokens: &[&str]) -> bool {
    if tokens.len() < 3 {
        return false;
    }
    let base = tokens[0].to_ascii_lowercase();
    if base != "git" || !tokens[1].eq_ignore_ascii_case("config") {
        return false;
    }
    const EXEC_KEYS: &[&str] = &[
        "core.sshcommand",
        "core.editor",
        "core.pager",
        "credential.helper",
        "diff.external",
        "core.fsmonitor",
    ];
    tokens[2..].iter().any(|token| {
        let lower = token.to_ascii_lowercase();
        // `--global core.sshCommand` and the attached `key=value` form both
        // land here; section prefixes like `--global` never match.
        let key = lower.split('=').next().unwrap_or("").trim_matches('-');
        EXEC_KEYS.contains(&key) || is_exec_capable_git_key(&lower)
    })
}

/// `git clone ext::sh -c '…'` executes the transport helper's command — a
/// positional token, so no flag check ever sees it.
fn has_executing_transport(tokens: &[&str]) -> bool {
    tokens
        .iter()
        .any(|token| token.starts_with("ext::") || token.starts_with("transport::"))
}

/// Commands whose *positional* tail (often after a `--` separator) executes
/// an arbitrary payload: `kubectl exec pod -- sh -c …`, `docker exec …`,
/// `docker run <image> <cmd>`, `go run ./pkg`, `npx <registry-pkg>`,
/// `python -m <module>`, `ssh host cmd`, `xargs cmd`, `sudo cmd`. An allow
/// rule never covers them whatever the arity table says — the same contract
/// as [`contains_exec_capable_flag`]. Without this, the arity collapse
/// (`docker run -v /:/host alpine sh` → `docker run`) let an allow rule
/// naming the prefix authorize the payload itself.
fn is_exec_by_positioning(tokens: &[&str]) -> bool {
    const CONTAINER_CARRIERS: &[&str] = &["kubectl", "docker", "podman", "nerdctl"];
    let base = tokens.first().copied().unwrap_or_default().to_ascii_lowercase();
    if tokens.len() >= 2
        && CONTAINER_CARRIERS.contains(&base.as_str())
        && (tokens[1].eq_ignore_ascii_case("exec") || tokens[1].eq_ignore_ascii_case("run"))
    {
        return true;
    }
    if tokens.len() >= 2 && matches!(base.as_str(), "go" | "cargo" | "bun" | "python" | "python3")
    {
        let sub = tokens[1].to_ascii_lowercase();
        let base_execs = match base.as_str() {
            "python" | "python3" => sub == "-m",
            _ => sub == "run",
        };
        if base_execs {
            return true;
        }
    }
    matches!(base.as_str(), "ssh" | "xargs" | "sudo" | "npx")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dict() -> BashArityDict {
        BashArityDict::new()
    }

    // ── classify ─────────────────────────────────────────────────────────────

    #[test]
    fn classify_git_status_bare() {
        assert_eq!(dict().classify(&["git", "status"]), "git status");
    }

    #[test]
    fn classify_git_status_with_short_flag() {
        assert_eq!(dict().classify(&["git", "status", "-s"]), "git status");
    }

    #[test]
    fn classify_git_status_with_long_flag() {
        assert_eq!(
            dict().classify(&["git", "status", "--porcelain"]),
            "git status"
        );
    }

    #[test]
    fn classify_git_push() {
        assert_eq!(
            dict().classify(&["git", "push", "origin", "main"]),
            "git push"
        );
    }

    #[test]
    fn classify_git_push_force() {
        assert_eq!(dict().classify(&["git", "push", "--force"]), "git push");
    }

    #[test]
    fn classify_npm_run_dev_arity_3() {
        assert_eq!(dict().classify(&["npm", "run", "dev"]), "npm run dev");
    }

    #[test]
    fn classify_npm_install() {
        assert_eq!(dict().classify(&["npm", "install"]), "npm install");
    }

    #[test]
    fn classify_cargo_check_with_flag() {
        assert_eq!(
            dict().classify(&["cargo", "check", "--workspace"]),
            "cargo check"
        );
    }

    #[test]
    fn classify_docker_compose_up_arity_3() {
        assert_eq!(
            dict().classify(&["docker", "compose", "up"]),
            "docker compose up"
        );
    }

    #[test]
    fn classify_kubectl_get_pods_arity_3() {
        assert_eq!(
            dict().classify(&["kubectl", "get", "pods"]),
            "kubectl get pods"
        );
    }

    #[test]
    fn classify_go_mod_tidy_arity_3() {
        assert_eq!(dict().classify(&["go", "mod", "tidy"]), "go mod tidy");
    }

    #[test]
    fn classify_make_no_subcommand() {
        assert_eq!(dict().classify(&["make", "all"]), "make");
    }

    #[test]
    fn classify_aws_s3_arity_3() {
        assert_eq!(dict().classify(&["aws", "s3", "ls"]), "aws s3 ls");
    }

    #[test]
    fn classify_terraform_plan() {
        assert_eq!(
            dict().classify(&["terraform", "plan", "-out=tfplan"]),
            "terraform plan"
        );
    }

    #[test]
    fn classify_unknown_falls_back_to_base() {
        assert_eq!(dict().classify(&["ls", "-la"]), "ls");
    }

    #[test]
    fn classify_empty_returns_empty() {
        assert_eq!(dict().classify(&[]), "");
    }

    // ── allow_rule_matches ────────────────────────────────────────────────────

    #[test]
    fn allow_rule_git_status_matches_with_flag() {
        assert!(dict().allow_rule_matches("git status", "git status -s"));
    }

    #[test]
    fn allow_rule_git_status_matches_porcelain() {
        assert!(dict().allow_rule_matches("git status", "git status --porcelain"));
    }

    #[test]
    fn allow_rule_git_status_does_not_match_push() {
        assert!(!dict().allow_rule_matches("git status", "git push origin main"));
    }

    #[test]
    fn allow_rule_git_status_does_not_match_checkout() {
        assert!(!dict().allow_rule_matches("git status", "git checkout main"));
    }

    #[test]
    fn allow_rule_npm_run_matches_dev() {
        assert!(dict().allow_rule_matches("npm run dev", "npm run dev"));
    }

    #[test]
    fn allow_rule_npm_run_dev_does_not_match_build() {
        assert!(!dict().allow_rule_matches("npm run dev", "npm run build"));
    }

    #[test]
    fn allow_rule_cargo_check_matches_with_flags() {
        assert!(dict().allow_rule_matches("cargo check", "cargo check --workspace"));
    }

    #[test]
    fn allow_rule_exact_match_still_works() {
        // A pattern not in the arity table falls back to exact/prefix match.
        assert!(dict().allow_rule_matches("ls", "ls -la"));
    }

    #[test]
    fn allow_rule_make_matches_with_target() {
        assert!(dict().allow_rule_matches("make", "make all"));
        assert!(dict().allow_rule_matches("make", "make clean"));
    }

    #[test]
    fn allow_rule_aws_s3_ls() {
        assert!(dict().allow_rule_matches("aws s3 ls", "aws s3 ls"));
        // "aws s3 cp" should not match "aws s3 ls"
        assert!(!dict().allow_rule_matches("aws s3 ls", "aws s3 cp src dst"));
    }

    #[test]
    fn allow_rule_bare_base_command_does_not_cover_subcommands() {
        // "git" has more specific arity entries, so it must not cover them.
        assert!(!dict().allow_rule_matches("git", "git push origin main"));
        assert!(!dict().allow_rule_matches("git", "git status"));
        // Bare invocation still matches, and commands without more specific
        // table entries keep the legacy prefix behaviour.
        assert!(dict().allow_rule_matches("git", "git"));
        assert!(dict().allow_rule_matches("ls", "ls -la"));
        assert!(dict().allow_rule_matches("make", "make all"));
    }

    #[test]
    fn allow_rule_exec_capable_flags_denied() {
        // Whatever the allow rule matches, a flag whose value executes a
        // command must not be authorized by it.
        assert!(!dict().allow_rule_matches(
            "git push",
            "git push --receive-pack=/bin/sh -c pwned origin"
        ));
        assert!(!dict().allow_rule_matches("git push", "git push --receive-pack /bin/sh origin"));
        assert!(!dict().allow_rule_matches("git fetch", "git fetch --upload-pack=/bin/sh origin"));
        assert!(!dict().allow_rule_matches("ssh", "ssh -oProxyCommand=touch$IFS/tmp/pwned host"));
        assert!(!dict().allow_rule_matches("ssh", "ssh -o ProxyCommand=cmd host"));
        assert!(!dict().allow_rule_matches("git push", "git -c core.sshCommand=/bin/sh push"));
        assert!(!dict().allow_rule_matches("find", "find . -exec rm {} ;"));
    }

    #[test]
    fn exec_by_positioning_is_never_allow_matched() {
        // `kubectl exec pod -- <payload>` executes the payload; an arity
        // allow rule must never cover it (same for docker exec / ssh /
        // xargs / sudo positional tails).
        assert!(!dict().allow_rule_matches("kubectl exec", "kubectl exec mypod -- sh -c id"));
        assert!(!dict().allow_rule_matches("docker exec", "docker exec ctr whoami"));
        assert!(!dict().allow_rule_matches("ssh", "ssh host uptime"));
        assert!(!dict().allow_rule_matches("xargs", "xargs rm"));
        assert!(!dict().allow_rule_matches("sudo", "sudo systemctl status nginx"));
        // Benign same-shape commands keep matching.
        assert!(dict().allow_rule_matches("git status", "git status --porcelain"));
        assert!(dict().allow_rule_matches("kubectl get", "kubectl get pods"));
    }

    // ── coverage count ────────────────────────────────────────────────────────

    #[test]
    fn dict_covers_at_least_30_commands() {
        // The issue requires 30+ common commands covered.
        assert!(
            BashArityDict::new().len() >= 30,
            "expected at least 30 entries, got {}",
            BashArityDict::new().len()
        );
    }

    // ── exec-by-positioning denial (security) ───────────────────────────────

    #[test]
    fn exec_by_positioning_vectors_never_match_allow_rules() {
        // Positional payloads: the arity collapse must not let an allow
        // rule naming the prefix authorize the payload itself.
        for (pattern, command) in [
            ("docker run", "docker run -v /:/host alpine sh"),
            ("kubectl exec", "kubectl exec pod -- rm -rf /"),
            ("go run", "go run ./exploit"),
            ("cargo run", "cargo run --bin evil"),
            ("bun run", "bun run evil.ts"),
            ("npx", "npx some-registry-pkg"),
            ("python -m", "python -m http.server"),
            ("ssh", "ssh host 'rm -rf /'"),
            ("xargs", "xargs rm -rf /"),
            ("sudo", "sudo rm -rf /"),
        ] {
            assert!(
                !dict().allow_rule_matches(pattern, command),
                "exec-by-positioning `{command}` must not match `{pattern}`"
            );
        }
    }

    #[test]
    fn exec_flag_families_and_git_c_keys_are_denied() {
        for command in [
            "find . -execdir rm {} +",
            "find . -exec rm {} ;",
            "git log --exec-path=/tmp/evil",
            "git push --receive-pack=/tmp/evil origin",
            "git -c core.sshCommand=/tmp/evil fetch",
            "git -c credential.helper='!sh -c /tmp/evil' push",
            "git -c diff.external=/tmp/evil diff",
            "git config --global core.sshCommand /tmp/evil",
            "git config credential.helper '!/tmp/evil'",
            "git clone ext::sh -c 'touch /tmp/pwned'",
            "ssh -oProxyCommand=/tmp/evil host",
        ] {
            assert!(
                !dict().allow_rule_matches("git", command)
                    && !dict().allow_rule_matches("git push", command)
                    && !dict().allow_rule_matches("git clone", command)
                    && !dict().allow_rule_matches("git config", command)
                    && !dict().allow_rule_matches("git log", command)
                    && !dict().allow_rule_matches("find", command)
                    && !dict().allow_rule_matches("ssh", command),
                "exec-capable `{command}` must never match an allow rule"
            );
        }
    }

    #[test]
    fn ordinary_git_config_still_matches() {
        assert!(dict().allow_rule_matches(
            "git config",
            "git config --global user.name Someone"
        ));
    }
}
