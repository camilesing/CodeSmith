//! `/pr <number|url>` — review a GitHub pull request in this session.
//!
//! `codesmith pr <N>` is the CLI door: it fetches the PR through `gh` and
//! *starts* a session with the PR as its first message. `/pr` is the same
//! fetch from inside a running session — the formatted PR (title, body,
//! branches, truncated diff) becomes the next prompt's text, so it reviews in
//! the conversation and workspace you already have.
//!
//! It stops at the prompt on purpose: the message is a draft you can edit
//! before sending, and nothing is fetched on a bare `/pr`.
//!
//! Requires the `gh` CLI on `PATH`; without it the command says so instead of
//! failing halfway. Known limitations (recorded beside the behaviour it owns):
//!
//! - GitHub only, through `gh`. Another forge's URL is rejected as
//!   unparseable rather than half-supported.
//! - `--checkout` runs `gh pr checkout`, which switches the branch of the
//!   workspace you are sitting in. It does not stash: a dirty tree is `gh`'s
//!   problem to report, and the failure is printed rather than swallowed.
//! - PR *comments* (`/pr-comments` in Claude Code, removed there in v2.1.91)
//!   are not fetched: ask the model to read them, which is what its removal
//!   message recommends.

use colored::Colorize;

use super::ReplState;

/// A parsed `/pr` argument line.
#[derive(Debug, PartialEq, Eq)]
pub struct Request {
    pub number: u32,
    /// `owner/name`, when the argument was a URL or `--repo` was given.
    pub repo: Option<String>,
    pub checkout: bool,
}

/// Parse `/pr`'s argument line.
///
/// `Ok(None)` means "no argument": the caller prints usage. Accepts `#123`,
/// `123`, and `https://github.com/owner/name/pull/123`, because the runner
/// windows this command is typed into contain all three.
pub fn parse(arg: &str) -> Result<Option<Request>, String> {
    let mut words = arg.split_whitespace();
    let mut number: Option<u32> = None;
    let mut repo: Option<String> = None;
    let mut checkout = false;

    while let Some(word) = words.next() {
        match word {
            "--repo" | "-R" => {
                let value = words
                    .next()
                    .ok_or_else(|| "--repo needs an owner/name argument".to_string())?;
                repo = Some(value.to_string());
            }
            "--checkout" | "-c" => checkout = true,
            _ if number.is_none() => {
                let (parsed, from_url) = parse_target(word)?;
                number = Some(parsed);
                if repo.is_none() {
                    repo = from_url;
                }
            }
            _ => return Err(format!("unexpected argument {word:?}")),
        }
    }

    Ok(number.map(|number| Request {
        number,
        repo,
        checkout,
    }))
}

/// A bare number, `#number`, or a GitHub PR URL.
fn parse_target(word: &str) -> Result<(u32, Option<String>), String> {
    let bare = word.trim_start_matches('#');
    if let Ok(number) = bare.parse::<u32>() {
        if number == 0 {
            return Err("PR numbers start at 1".to_string());
        }
        return Ok((number, None));
    }

    let rest = word
        .strip_prefix("https://")
        .or_else(|| word.strip_prefix("http://"))
        .ok_or_else(|| format!("not a PR number or GitHub URL: {word:?}"))?;
    let rest = rest.strip_prefix("www.").unwrap_or(rest);
    let rest = rest
        .strip_prefix("github.com/")
        .ok_or_else(|| format!("only github.com pull requests are supported: {word:?}"))?;
    let mut segments = rest.trim_end_matches('/').split('/');
    let owner = segments.next().unwrap_or_default();
    let name = segments.next().unwrap_or_default();
    let kind = segments.next().unwrap_or_default();
    let number = segments.next().unwrap_or_default();
    if owner.is_empty() || name.is_empty() || kind != "pull" {
        return Err(format!(
            "expected https://github.com/<owner>/<repo>/pull/<n>, got {word:?}"
        ));
    }
    // `.../pull/123/files?diff=split` and friends: keep the leading digits.
    let digits: String = number.chars().take_while(char::is_ascii_digit).collect();
    let number: u32 = digits
        .parse()
        .map_err(|_| format!("no PR number in {word:?}"))?;
    Ok((number, Some(format!("{owner}/{name}"))))
}

/// `/pr <number|url> [--repo owner/name] [--checkout]`.
pub async fn run(state: &mut ReplState, arg: &str) {
    let request = match parse(arg) {
        Ok(Some(request)) => request,
        Ok(None) => {
            println!(
                "{}",
                "usage: /pr <number|url> [--repo owner/name] [--checkout] — fetches a GitHub PR into the prompt"
                    .dimmed()
            );
            return;
        }
        Err(err) => {
            println!("{} {err}", "✗".red());
            return;
        }
    };

    if !crate::main_root::is_command_available("gh") {
        println!(
            "{} /pr needs the GitHub CLI: install it (https://cli.github.com) and run `gh auth login`",
            "✗".red()
        );
        return;
    }

    let number = request.number;
    let repo = request.repo.clone();
    // `gh` runs as a child process and reads its output to the end — blocking
    // IO that must not sit on the async runtime.
    let fetched = tokio::task::spawn_blocking(move || {
        let view = crate::main_root::run_gh_pr_view(number, repo.as_deref())?;
        let diff = crate::main_root::run_gh_pr_diff(number, repo.as_deref())?;
        let checkout = if request.checkout {
            crate::main_root::run_gh_pr_checkout(number, repo.as_deref()).err()
        } else {
            None
        };
        Ok::<_, anyhow::Error>((view, diff, checkout))
    })
    .await;

    let (view, diff, checkout_error) = match fetched {
        Ok(Ok(fetched)) => fetched,
        Ok(Err(err)) => {
            println!("{} /pr #{number} failed: {err}", "✗".red());
            return;
        }
        Err(err) => {
            println!("{} /pr could not reach `gh`: {err}", "✗".red());
            return;
        }
    };

    let (added, removed) = diff_stat(&diff);
    println!("{} {}", format!("#{number}").bold(), view.title.as_str());
    println!(
        "{}",
        format!(
            "  {} ← {} · {added} added / {removed} removed lines",
            view.head, view.base
        )
        .dimmed()
    );
    println!("{}", format!("  {}", view.url).dimmed());
    match checkout_error {
        None if request.checkout => println!("{}", "  checked out into this workspace".dimmed()),
        Some(err) => println!(
            "{}",
            format!("warning: gh pr checkout failed ({err}); the prompt is still prepared")
                .yellow()
        ),
        None => {}
    }

    state.next_input = Some(crate::main_root::format_pr_prompt(number, &view, &diff));
    println!(
        "{}",
        "— the PR is in the prompt: press Enter to send it, or edit it first".dimmed()
    );
}

/// Lines a unified diff adds and removes, ignoring the `+++`/`---` headers.
fn diff_stat(diff: &str) -> (usize, usize) {
    let mut added = 0;
    let mut removed = 0;
    for line in diff.lines() {
        if line.starts_with("+++") || line.starts_with("---") {
            continue;
        }
        if line.starts_with('+') {
            added += 1;
        } else if line.starts_with('-') {
            removed += 1;
        }
    }
    (added, removed)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn numbers_hashes_and_urls_all_name_a_pr() {
        assert_eq!(
            parse("123").expect("number").expect("some"),
            Request {
                number: 123,
                repo: None,
                checkout: false
            }
        );
        assert_eq!(parse("#7").expect("hash").expect("some").number, 7);
        let url = parse("https://github.com/o/r/pull/451/files?diff=split")
            .expect("url")
            .expect("some");
        assert_eq!(url.number, 451);
        assert_eq!(url.repo.as_deref(), Some("o/r"));
    }

    #[test]
    fn flags_are_read_before_and_after_the_target() {
        let req = parse("--repo o/r --checkout 12")
            .expect("flags")
            .expect("some");
        assert_eq!(req.repo.as_deref(), Some("o/r"));
        assert!(req.checkout);
        let req = parse("12 --checkout")
            .expect("trailing flag")
            .expect("some");
        assert!(req.checkout);
        // A URL carries its own repo, which `--repo` may still override.
        let req = parse("https://github.com/o/r/pull/9 --repo other/name")
            .expect("override")
            .expect("some");
        assert_eq!(req.repo.as_deref(), Some("other/name"));
    }

    #[test]
    fn a_bare_pr_is_usage_not_an_error() {
        assert!(parse("").expect("empty").is_none());
        assert!(parse("   ").expect("blank").is_none());
    }

    #[test]
    fn other_forges_and_junk_are_refused_by_name() {
        let err = parse("https://gitlab.com/o/r/pull/3").expect_err("gitlab");
        assert!(err.contains("github.com"), "{err}");
        let err = parse("some-branch").expect_err("junk");
        assert!(err.contains("some-branch"), "{err}");
        assert!(parse("#0").is_err(), "PR numbers start at 1");
        assert!(parse("12 --repo").is_err(), "--repo needs a value");
        assert!(parse("12 13").is_err(), "a second target is a typo");
    }

    #[test]
    fn diff_stat_counts_only_content_lines() {
        let diff = "--- a/x\n+++ b/x\n@@ -1 +1,2 @@\n-old\n+new\n+more\n";
        assert_eq!(diff_stat(diff), (2, 1));
    }
}
