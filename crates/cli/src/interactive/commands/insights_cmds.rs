//! `/insights [7d|30d|all]` — what the local stores say about how you work.
//!
//! The numbers come from the same three sources `codesmith metrics` reads
//! (`~/.codesmith/audit.log`, `~/.codesmith/sessions/*.json`, the runtime
//! thread event streams) through [`crate::metrics::collect`], so the two
//! cannot disagree. `/insights` differs in *shape*: a short digest in the
//! transcript plus a self-contained HTML report written next to the stores,
//! which is what Claude Code's `/insights` produces.
//!
//! Everything here is local. Nothing is uploaded, and the report is a file
//! under `~/.codesmith/insights/` that only you read.
//!
//! Known limitations (recorded beside the behaviour it owns):
//!
//! - The report is generated from files, not from a model, so it has no
//!   prose analysis of *why* a session went badly — it shows the counters
//!   (failed tool calls, capacity interventions, compaction) that point at
//!   the sessions worth re-reading.
//! - Sessions whose metadata does not carry a workspace or model are counted
//!   in the totals but appear in no per-project / per-model row.
//! - The window filter applies to session timestamps and audit-log `ts`
//!   fields only; a session file updated inside the window contributes all of
//!   its history, not just its messages from the window.

use std::collections::HashMap;
use std::path::PathBuf;

use chrono::{DateTime, Utc};
use colored::Colorize;

use crate::metrics::{Rollup, ToolStats};

/// What a tool-usage row shows in the digest.
const TOP_TOOLS: usize = 10;
/// What the HTML report shows.
const HTML_TOP_TOOLS: usize = 20;

/// Parse the optional window argument.
///
/// `None` means "all of it" — the rollup then spans every record on disk.
fn window_of(arg: &str) -> Result<Option<DateTime<Utc>>, String> {
    let arg = arg.trim();
    if arg.is_empty() || arg == "30d" {
        return crate::metrics::parse_since("30d")
            .map(Some)
            .map_err(|err| err.to_string());
    }
    if arg.eq_ignore_ascii_case("all") || arg == "--all" {
        return Ok(None);
    }
    crate::metrics::parse_since(arg)
        .map(Some)
        .map_err(|_| format!("unrecognised window {arg:?} — use 7d, 30d, 24h, or all"))
}

/// The report directory, beside the other `~/.codesmith` stores.
fn report_dir() -> PathBuf {
    codesmith_config::codesmith_home()
        .map(|home| home.join("insights"))
        .unwrap_or_else(|_| {
            dirs::home_dir()
                .unwrap_or_else(|| PathBuf::from("."))
                .join(".codesmith")
                .join("insights")
        })
}

/// The busiest tools, by call count.
fn top_tools(map: &HashMap<String, ToolStats>, limit: usize) -> Vec<(&String, &ToolStats)> {
    let mut rows: Vec<(&String, &ToolStats)> = map.iter().collect();
    rows.sort_by(|a, b| b.1.calls.cmp(&a.1.calls).then_with(|| a.0.cmp(b.0)));
    rows.truncate(limit);
    rows
}

fn top<V: Ord>(map: &HashMap<String, V>, limit: usize) -> Vec<(&String, &V)> {
    let mut rows: Vec<(&String, &V)> = map.iter().collect();
    rows.sort_by(|a, b| b.1.cmp(a.1).then_with(|| a.0.cmp(b.0)));
    rows.truncate(limit);
    rows
}

fn approvals(rollup: &Rollup) -> (u64, u64) {
    (
        rollup.tools.values().map(|t| t.auto_approved).sum(),
        rollup.tools.values().map(|t| t.prompted).sum(),
    )
}

fn failures(rollup: &Rollup) -> u64 {
    rollup.tools.values().map(|t| t.failures).sum()
}

/// How the window is named in both renderings.
fn window_label(arg: &str) -> String {
    let arg = arg.trim();
    if arg.is_empty() || arg == "30d" {
        "last 30d".to_string()
    } else if arg.eq_ignore_ascii_case("all") || arg == "--all" {
        "all time".to_string()
    } else {
        format!("last {arg}")
    }
}

/// `/insights [7d|30d|all]`.
pub async fn run(arg: &str) {
    let since = match window_of(arg) {
        Ok(since) => since,
        Err(err) => {
            println!("{} {err}", "✗".red());
            return;
        }
    };

    // Reading the stores is blocking file IO, and the log files are the
    // biggest thing this command touches.
    let rollup = match tokio::task::spawn_blocking(move || crate::metrics::collect(since)).await {
        Ok(rollup) => rollup,
        Err(err) => {
            println!("{} /insights could not read the stores: {err}", "✗".red());
            return;
        }
    };

    let window = window_label(arg);
    print_digest(&rollup, &window);

    let html = render_html(&rollup, &window);
    let dir = report_dir();
    let stamp = chrono::Local::now().format("%Y%m%d-%H%M%S");
    let path = dir.join(format!("insights-{stamp}.html"));
    // `create_dir_all` then `write` — both through tokio, since this is
    // reached from an async dispatch and the report can be megabytes.
    let write = async {
        tokio::fs::create_dir_all(&dir).await?;
        tokio::fs::write(&path, html.as_bytes()).await
    };
    match write.await {
        Ok(()) => println!(
            "{}",
            format!("report written to {}", path.display()).dimmed()
        ),
        Err(err) => println!(
            "{}",
            format!(
                "warning: the HTML report could not be written to {}: {err}",
                path.display()
            )
            .yellow()
        ),
    }
}

fn print_digest(rollup: &Rollup, window: &str) {
    println!("{} {}", "insights".bold(), format!("· {window}").dimmed());
    println!(
        "  {}            {} sessions · {} messages · {} tokens",
        "activity".dimmed(),
        crate::metrics::fmt_num(rollup.sessions),
        crate::metrics::fmt_num(rollup.messages),
        crate::metrics::fmt_num(rollup.tokens),
    );
    match (rollup.earliest_ts, rollup.latest_ts) {
        (Some(start), Some(end)) => println!(
            "  {}             {} → {} ({} days)",
            "period".dimmed(),
            start.format("%Y-%m-%d"),
            end.format("%Y-%m-%d"),
            (end - start).num_days().max(0),
        ),
        _ => println!(
            "  {}             (no timestamped records)",
            "period".dimmed()
        ),
    }

    let projects = top(&rollup.projects, 5);
    if projects.is_empty() {
        println!(
            "  {}           (no session records with a workspace)",
            "projects".dimmed()
        );
    } else {
        let shown = projects
            .iter()
            .map(|(path, count)| format!("{} ({count})", short_path(path)))
            .collect::<Vec<_>>()
            .join(", ");
        println!(
            "  {}           {} distinct — {shown}",
            "projects".dimmed(),
            rollup.projects.len()
        );
    }

    let models = top(&rollup.models, 4);
    if !models.is_empty() {
        let shown = models
            .iter()
            .map(|(model, count)| format!("{model} ×{count}"))
            .collect::<Vec<_>>()
            .join(", ");
        println!("  {}             {shown}", "models".dimmed());
    }

    let calls = rollup.total_tool_calls();
    if calls == 0 {
        println!(
            "  {}              (no tool calls recorded)",
            "tools".dimmed()
        );
    } else {
        let (auto, prompted) = approvals(rollup);
        let rate = match success_rate(rollup) {
            Some(pct) => format!("{pct:.1}% success"),
            None => "no result data".to_string(),
        };
        println!(
            "  {}              {} calls — {rate}; {auto} auto-approved, {prompted} prompted, {} failed",
            "tools".dimmed(),
            crate::metrics::fmt_num(calls),
            crate::metrics::fmt_num(failures(rollup)),
        );
        for (name, stats) in top_tools(&rollup.tools, TOP_TOOLS) {
            println!(
                "    {:<22} {:>6}  {}",
                name,
                crate::metrics::fmt_num(stats.calls),
                tool_note(stats)
            );
        }
        if rollup.tools.len() > TOP_TOOLS {
            println!("    … and {} more", rollup.tools.len() - TOP_TOOLS);
        }
    }

    if rollup.compaction.events > 0 || rollup.agents.spawns > 0 || rollup.capacity.total > 0 {
        println!(
            "  {}           {} compactions · {} sub-agents · {} capacity interventions",
            "context".dimmed(),
            crate::metrics::fmt_num(rollup.compaction.events),
            crate::metrics::fmt_num(rollup.agents.spawns),
            crate::metrics::fmt_num(rollup.capacity.total),
        );
    }
    if rollup.capacity.total > 0 {
        let cats = top(&rollup.capacity.by_category, 4)
            .iter()
            .map(|(kind, count)| format!("{count} {kind}"))
            .collect::<Vec<_>>()
            .join(", ");
        println!(
            "{}",
            format!("  capacity interventions: {cats} — those turns hit a limit, not a bug")
                .dimmed()
        );
    }
    println!(
        "{}",
        "  full numbers: `codesmith metrics` · per-turn cost: /cost · this session: /tokens"
            .dimmed()
    );
}

fn tool_note(stats: &ToolStats) -> String {
    match stats.success_rate_pct() {
        Some(pct) => {
            let mut note = format!("{pct:5.1}% success");
            if stats.failures > 0 {
                note.push_str(&format!(" ({} failed)", stats.failures));
            }
            note
        }
        None => {
            let mut note = String::new();
            if stats.auto_approved > 0 {
                note.push_str(&format!("auto×{}", stats.auto_approved));
            }
            if stats.prompted > 0 {
                if !note.is_empty() {
                    note.push_str(", ");
                }
                note.push_str(&format!("prompted×{}", stats.prompted));
            }
            note
        }
    }
}

fn success_rate(rollup: &Rollup) -> Option<f64> {
    let ok: u64 = rollup.tools.values().map(|t| t.successes).sum();
    let judged: u64 = ok + failures(rollup);
    (judged > 0).then(|| ok as f64 / judged as f64 * 100.0)
}

/// The last two path components, which is how a project is recognisable.
fn short_path(path: &str) -> String {
    let parts: Vec<&str> = path.trim_end_matches('/').rsplit('/').take(2).collect();
    parts.into_iter().rev().collect::<Vec<_>>().join("/")
}

fn escape(text: &str) -> String {
    text.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

fn render_html(rollup: &Rollup, window: &str) -> String {
    let mut body = String::new();
    let generated = chrono::Local::now().to_rfc3339();

    let mut cards = String::new();
    for (label, value) in [
        ("sessions", rollup.sessions),
        ("messages", rollup.messages),
        ("tokens", rollup.tokens),
        ("tool calls", rollup.total_tool_calls()),
    ] {
        cards.push_str(&format!(
            "<div class=card><span class=num>{}</span><span class=label>{}</span></div>",
            crate::metrics::fmt_num(value),
            label
        ));
    }

    let mut projects = String::new();
    for (path, count) in top(&rollup.projects, 25) {
        projects.push_str(&format!(
            "<tr><td>{}</td><td class=n>{count}</td></tr>",
            escape(path)
        ));
    }
    if projects.is_empty() {
        projects.push_str(
            "<tr><td colspan=2 class=empty>no session records with a workspace</td></tr>",
        );
    }

    let mut models = String::new();
    for (model, count) in top(&rollup.models, 25) {
        models.push_str(&format!(
            "<tr><td>{}</td><td class=n>{count}</td></tr>",
            escape(model)
        ));
    }

    let mut tools = String::new();
    for (name, stats) in top_tools(&rollup.tools, HTML_TOP_TOOLS) {
        tools.push_str(&format!(
            "<tr><td>{}</td><td class=n>{}</td><td class=n>{}</td><td class=n>{}</td><td>{}</td></tr>",
            escape(name),
            crate::metrics::fmt_num(stats.calls),
            stats.auto_approved,
            stats.prompted,
            escape(&tool_note(stats)),
        ));
    }
    if tools.is_empty() {
        tools.push_str("<tr><td colspan=5 class=empty>no tool calls recorded</td></tr>");
    }

    // "Where things went wrong": the counters that point at sessions worth
    // re-reading, rather than a model's guess at why.
    let mut wrong = Vec::new();
    let failed = failures(rollup);
    if failed > 0 {
        let mut worst = rollup
            .tools
            .iter()
            .filter(|(_, stats)| stats.failures > 0)
            .collect::<Vec<_>>();
        worst.sort_by_key(|(_, stats)| std::cmp::Reverse(stats.failures));
        let names = worst
            .iter()
            .take(5)
            .map(|(name, stats)| format!("{} ({})", escape(name), stats.failures))
            .collect::<Vec<_>>()
            .join(", ");
        wrong.push(format!("{failed} failed tool calls — {names}"));
    }
    if rollup.capacity.total > 0 {
        let cats = top(&rollup.capacity.by_category, 5)
            .iter()
            .map(|(kind, count)| format!("{count} {}", escape(kind)))
            .collect::<Vec<_>>()
            .join(", ");
        wrong.push(format!(
            "{} capacity interventions ({cats}) — turns that hit a provider limit",
            rollup.capacity.total
        ));
    }
    if rollup.compaction.events > 0 {
        let avg = rollup
            .compaction
            .avg_reduction_pct()
            .map(|pct| format!(", avg {pct:.0}% smaller"))
            .unwrap_or_default();
        wrong.push(format!(
            "{} compactions — the context filled before the work was done{avg}",
            rollup.compaction.events
        ));
    }
    let (auto, prompted) = approvals(rollup);
    if prompted > 0 {
        wrong.push(format!(
            "{prompted} approval prompts ({auto} calls auto-approved) — /permissions can widen what runs unattended"
        ));
    }
    if wrong.is_empty() {
        wrong.push("nothing recorded as failing in this window".to_string());
    }

    let try_this = [
        ("/permissions list", "what runs without asking, and why"),
        ("/rewind", "roll the tree back to a checkpoint"),
        ("/insights all", "this report over every record on disk"),
        ("/tasks", "background work: jobs, sub-agents, durable tasks"),
        (
            "/memory auto",
            "whether memories are being recorded, and by which level",
        ),
    ];
    let mut suggestions = String::new();
    for (command, why) in try_this {
        suggestions.push_str(&format!(
            "<li><code>{}</code> — {}</li>",
            escape(command),
            escape(why)
        ));
    }

    let period = match (rollup.earliest_ts, rollup.latest_ts) {
        (Some(start), Some(end)) => format!(
            "{} → {} ({} days)",
            start.format("%Y-%m-%d"),
            end.format("%Y-%m-%d"),
            (end - start).num_days().max(0)
        ),
        _ => "no timestamped records".to_string(),
    };

    body.push_str(&format!(
        "<h2>Activity</h2><div class=cards>{cards}</div>\
         <p class=dim>Period: {period} · generated {generated} · window: {}</p>",
        escape(window)
    ));
    body.push_str(&format!(
        "<h2>Where things went wrong</h2><ul>{}</ul>",
        wrong
            .iter()
            .map(|line| format!("<li>{line}</li>"))
            .collect::<Vec<_>>()
            .join("")
    ));
    body.push_str(&format!(
        "<h2>Projects</h2><table><tr><th>workspace</th><th>sessions</th></tr>{projects}</table>"
    ));
    body.push_str(&format!(
        "<h2>Models</h2><table><tr><th>model</th><th>sessions</th></tr>{models}</table>"
    ));
    body.push_str(&format!(
        "<h2>Tools</h2><table><tr><th>tool</th><th>calls</th><th>auto</th><th>prompted</th><th>result</th></tr>{tools}</table>"
    ));
    body.push_str(&format!("<h2>Worth trying</h2><ul>{suggestions}</ul>"));
    body.push_str(&format!(
        "<h2>Sub-agents &amp; credentials</h2><p>{} sub-agent spawns ({} ok, {} failed) · \
         {} credential saves, {} clears · {} parsed of {} lines read.</p>",
        rollup.agents.spawns,
        rollup.agents.successes,
        rollup.agents.failures,
        rollup.credentials.saves,
        rollup.credentials.clears,
        rollup.parsed_lines,
        rollup.total_lines,
    ));

    format!(
        "<!doctype html><html lang=en><meta charset=utf-8>\
         <title>CodeSmith insights</title>\
         <style>\
         body{{font:14px/1.5 -apple-system,system-ui,sans-serif;margin:2rem auto;max-width:56rem;padding:0 1rem;color:#1c1c1e}}\
         h1{{font-size:1.4rem}} h2{{font-size:1rem;margin-top:2rem;border-bottom:1px solid #ddd;padding-bottom:.25rem}}\
         table{{border-collapse:collapse;width:100%}} th,td{{text-align:left;padding:.25rem .5rem;border-bottom:1px solid #eee;vertical-align:top}}\
         th{{color:#666;font-weight:600}} .n{{text-align:right;font-variant-numeric:tabular-nums;white-space:nowrap}}\
         .cards{{display:flex;gap:1rem;flex-wrap:wrap}} .card{{border:1px solid #ddd;border-radius:8px;padding:.6rem 1rem;min-width:6rem}}\
         .num{{display:block;font-size:1.3rem;font-weight:600}} .label{{color:#666}} .dim,.empty{{color:#666}} code{{background:#f4f4f5;padding:.05rem .3rem;border-radius:4px}}\
         </style><body><h1>CodeSmith insights</h1>{body}</body></html>"
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn window_defaults_to_thirty_days_and_accepts_all() {
        let default = window_of("").expect("default window");
        assert!(default.is_some(), "no argument means the 30d window");
        assert!(window_of("all").expect("all").is_none());
        assert!(window_of("--all").expect("--all").is_none());
        assert!(window_of("7d").expect("7d").is_some());
    }

    #[test]
    fn an_unusable_window_says_so_instead_of_guessing() {
        let err = window_of("last-tuesday").expect_err("must not be accepted");
        assert!(err.contains("7d"), "{err}");
        assert!(
            window_of("0m").is_err(),
            "a zero window is a typo, not a window"
        );
    }

    #[test]
    fn short_path_keeps_the_recognisable_tail() {
        assert_eq!(short_path("/Users/x/dev/CodeSmith"), "dev/CodeSmith");
        assert_eq!(short_path("/tmp"), "/tmp");
    }

    #[test]
    fn html_escapes_what_it_prints() {
        assert_eq!(escape("<a>&\"b\""), "&lt;a&gt;&amp;&quot;b&quot;");
    }

    #[test]
    fn the_report_carries_the_counters_it_claims() {
        let mut rollup = Rollup {
            sessions: 3,
            messages: 40,
            tokens: 9_000,
            ..Rollup::default()
        };
        rollup.projects.insert("/Users/x/dev/CodeSmith".into(), 2);
        rollup.models.insert("deepseek-chat".into(), 3);
        rollup.tools.insert(
            "exec_shell".into(),
            ToolStats {
                calls: 7,
                failures: 2,
                successes: 5,
                ..ToolStats::default()
            },
        );
        let html = render_html(&rollup, "all time");
        assert!(html.contains("CodeSmith insights"));
        assert!(html.contains("/Users/x/dev/CodeSmith"));
        assert!(html.contains("deepseek-chat"));
        assert!(html.contains("failed tool calls"));
        // The digest and the report read one rollup, so both must mention the
        // same session count.
        assert!(
            html.contains(">3<"),
            "session count missing from the report"
        );
    }
}
