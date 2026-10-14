//! `/loop [interval] [prompt]` — run a prompt again and again.
//!
//! A loop is a stored [automation] — the same durable record the `automation`
//! tools write and the scheduler the REPL already starts ticks over — so the
//! prompt is re-run by the background-task substrate (`EngineTaskExecutor`
//! through a runtime thread), not by this transcript's turn loop. Each
//! iteration is a task with its own thread, which is why `/loop list` shows
//! the last run's status and `/tasks` shows the runs themselves.
//!
//! That difference is the load-bearing one to know about, so it is printed
//! when a loop is created: **the iterations do not appear in this
//! conversation.** Ask the loop to write its findings somewhere the session
//! can read (a file, a note) if the results should come back here.
//!
//! The interval is the scheduler's, not Claude Code's: `AutomationSchedule`
//! understands `FREQ=HOURLY` (with `INTERVAL`, `BYDAY`) and `FREQ=WEEKLY`,
//! so the floor is one hour. `/loop 5m` is refused by name rather than
//! silently rounded up to an hour.
//!
//! Known limitations (recorded beside the behaviour it owns):
//!
//! - No self-pacing: Claude Code lets the model choose the wait between
//!   iterations when the interval is omitted. Here an interval is required
//!   (or `loop.md`'s cadence is not read at all — only its prompt is).
//! - The run's output lands in its runtime thread. There is no "the loop's
//!   latest result" panel; `/loop list` reports status, and the thread's
//!   transcript is in `~/.codesmith/tasks/runtime/`.
//! - Iterations stop when the automation is deleted or paused; closing this
//!   REPL does not stop them (the scheduler belongs to the session, but the
//!   next session picks the same store up).
//!
//! [automation]: crate::automation_manager

use colored::Colorize;

use super::ReplState;
use crate::automation_manager::{
    AutomationRecord, AutomationRunStatus, AutomationSchedule, AutomationStatus,
    CreateAutomationRequest,
};

/// Name prefix that marks a record as created by `/loop`.
const NAME_PREFIX: &str = "loop:";

/// Used when `/loop <interval>` carries no prompt and the workspace has no
/// `loop.md`. Claude Code documents a built-in maintenance prompt without
/// quoting it; this is CodeSmith's own wording.
const MAINTENANCE_PROMPT: &str = "Check the work in progress for anything now broken: run the \
project's formatter, linter, and test suite, fix what plainly failed, and report what you \
changed and what still fails. Do not start new features.";

/// Translate `/loop`'s interval into an RRULE the scheduler accepts.
pub fn interval_to_rrule(interval: &str) -> Result<String, String> {
    let raw = interval.trim();
    if raw.is_empty() {
        return Err("an interval is required, e.g. /loop 2h <prompt>".to_string());
    }
    if raw
        .get(..5)
        .is_some_and(|head| head.eq_ignore_ascii_case("FREQ="))
    {
        // Validate at the type that will have to parse it later, so a typo
        // fails here (where the user is looking) and not on the first tick.
        AutomationSchedule::parse_rrule(raw).map_err(|err| err.to_string())?;
        return Ok(raw.to_ascii_uppercase());
    }
    if raw.eq_ignore_ascii_case("hourly") {
        return Ok("FREQ=HOURLY".to_string());
    }
    if raw.eq_ignore_ascii_case("daily") {
        return Ok("FREQ=HOURLY;INTERVAL=24".to_string());
    }

    let (digits, unit) = raw.split_at(raw.find(|c: char| !c.is_ascii_digit()).unwrap_or(raw.len()));
    let count: u32 = digits
        .parse()
        .map_err(|_| format!("unrecognised interval {raw:?} — use 2h, 1d, hourly, or FREQ=…"))?;
    if count == 0 {
        return Err("an interval of zero would run continuously".to_string());
    }
    match unit {
        "h" | "H" => Ok(if count == 1 {
            "FREQ=HOURLY".to_string()
        } else {
            format!("FREQ=HOURLY;INTERVAL={count}")
        }),
        "d" | "D" => {
            // `count` is any user-supplied number: `count * 24` overflows
            // u32 (panic in debug, silent wrap in release — the wrapped
            // value can even reach INTERVAL=0, i.e. fire-every-tick).
            let hours = count.checked_mul(24).ok_or_else(|| {
                format!("{raw} is too long an interval to schedule — the maximum is 178956970d")
            })?;
            Ok(format!("FREQ=HOURLY;INTERVAL={hours}"))
        }
        "m" | "M" | "s" | "S" => Err(format!(
            "{raw} is below the scheduler's floor of one hour — use 1h or more, \
             or a `sleep` loop in a shell for finer cadence"
        )),
        _ => Err(format!(
            "unrecognised interval {raw:?} — use 2h, 1d, hourly, or an RRULE like FREQ=HOURLY;INTERVAL=2"
        )),
    }
}

/// The prompt for a bare `/loop <interval>`: `loop.md` if the workspace has
/// one, else the built-in maintenance prompt.
async fn prompt_for(workspace: &std::path::Path) -> String {
    let path = workspace.join("loop.md");
    match tokio::fs::read_to_string(&path).await {
        Ok(text) if !text.trim().is_empty() => {
            println!("{}", format!("— using {}", path.display()).dimmed());
            text.trim().to_string()
        }
        _ => {
            println!(
                "{}",
                "— no prompt and no loop.md: using the built-in maintenance prompt".dimmed()
            );
            MAINTENANCE_PROMPT.to_string()
        }
    }
}

/// A loop name built from the prompt's first line, so `/loop list` is readable.
fn name_for(prompt: &str) -> String {
    let first = prompt.lines().next().unwrap_or(prompt).trim();
    let mut name = format!("{NAME_PREFIX} {first}");
    if name.chars().count() > 60 {
        name = name.chars().take(57).collect::<String>() + "…";
    }
    name
}

/// Address a record by full id, id prefix, or name.
fn resolve<'a>(
    records: &'a [AutomationRecord],
    needle: &str,
) -> Result<&'a AutomationRecord, String> {
    if let Some(exact) = records
        .iter()
        .find(|record| record.id == needle || record.name.eq_ignore_ascii_case(needle))
    {
        return Ok(exact);
    }
    let matches: Vec<&AutomationRecord> = records
        .iter()
        .filter(|record| record.id.starts_with(needle) && needle.len() >= 4)
        .collect();
    match matches.len() {
        1 => Ok(matches[0]),
        0 => Err(format!("no loop or automation matches {needle:?}")),
        _ => Err(format!(
            "{needle:?} matches {} automations — use more of the id or the whole name",
            matches.len()
        )),
    }
}

fn when(value: Option<chrono::DateTime<chrono::Utc>>) -> String {
    match value {
        Some(ts) => ts
            .with_timezone(&chrono::Local)
            .format("%Y-%m-%d %H:%M")
            .to_string(),
        None => "—".to_string(),
    }
}

fn run_status(status: AutomationRunStatus) -> &'static str {
    match status {
        AutomationRunStatus::Queued => "queued",
        AutomationRunStatus::Running => "running",
        AutomationRunStatus::Completed => "completed",
        AutomationRunStatus::Failed => "failed",
        AutomationRunStatus::Canceled => "canceled",
    }
}

/// `/loop [interval] [prompt]`.
///
/// Returns after printing: loops are stored, so nothing here waits on a run.
pub async fn run(state: &mut ReplState, arg: &str) {
    let Some(automations) = state.runtime_services.automations.clone() else {
        println!(
            "{}",
            "✗ the automation store is not available in this session".red()
        );
        return;
    };

    let mut words = arg.split_whitespace();
    let first = words.next().unwrap_or_default();
    let rest = words.collect::<Vec<_>>().join(" ");

    match first.to_ascii_lowercase().as_str() {
        "" | "list" | "status" => list(&automations).await,
        "stop" | "delete" | "rm" | "off" => stop(&automations, &rest).await,
        "pause" | "resume" => set_paused(&automations, &rest, first).await,
        "run" | "now" => run_now(state, &automations, &rest).await,
        interval => {
            let rrule = match interval_to_rrule(interval) {
                Ok(rrule) => rrule,
                Err(err) => {
                    println!("{} {err}", "✗".red());
                    return;
                }
            };
            let prompt = if rest.trim().is_empty() {
                prompt_for(&state.workspace).await
            } else {
                rest.clone()
            };
            create(state, &automations, &rrule, &prompt).await;
        }
    }
}

async fn create(
    state: &ReplState,
    automations: &std::sync::Arc<dyn codesmith_agent_runtime::host_services::AutomationManagerHost>,
    rrule: &str,
    prompt: &str,
) {
    let request = CreateAutomationRequest {
        name: name_for(prompt),
        prompt: prompt.to_string(),
        rrule: rrule.to_string(),
        cwds: vec![state.workspace.clone()],
        status: None,
    };
    match automations.create_automation(request).await {
        Ok(record) => {
            println!(
                "{} {}",
                "loop".bold(),
                format!("· {} · {}", record.rrule, when(record.next_run_at)).dimmed()
            );
            println!("  {} {}", "id".dimmed(), record.id);
            println!("  {} {}", "prompt".dimmed(), first_line(prompt));
            println!(
                "{}",
                "  runs as a background task (its own thread) — iterations do not appear in this \
                 conversation; /loop list shows status"
                    .dimmed()
            );
        }
        Err(err) => println!("{} /loop could not be stored: {err}", "✗".red()),
    }
}

async fn list(
    automations: &std::sync::Arc<dyn codesmith_agent_runtime::host_services::AutomationManagerHost>,
) {
    let records = match automations.list_automations().await {
        Ok(records) => records,
        Err(err) => {
            println!("{} could not read the automations: {err}", "✗".red());
            return;
        }
    };
    if records.is_empty() {
        println!(
            "{}",
            "no loops or automations — /loop 2h <prompt> starts one".dimmed()
        );
        return;
    }
    println!("{}", format!("{} automation(s)", records.len()).bold());
    for record in &records {
        let status = match record.status {
            AutomationStatus::Active => "active".green().to_string(),
            AutomationStatus::Paused => "paused".yellow().to_string(),
        };
        let last = match automations.list_runs(&record.id, Some(1)).await {
            Ok(runs) => runs.first().map_or("never run".to_string(), |run| {
                format!(
                    "last {} at {}",
                    run_status(run.status),
                    when(Some(run.created_at))
                )
            }),
            Err(err) => format!("run history unreadable: {err}"),
        };
        let short: String = record.id.chars().take(8).collect();
        println!(
            "  {} {} {} {} · next {} · {last}",
            short.dimmed(),
            record.name,
            record.rrule.dimmed(),
            status,
            when(record.next_run_at)
        );
    }
    println!(
        "{}",
        "  /loop run <id> fires one now · /loop pause|resume <id> · /loop stop <id>".dimmed()
    );
}

async fn stop(
    automations: &std::sync::Arc<dyn codesmith_agent_runtime::host_services::AutomationManagerHost>,
    needle: &str,
) {
    if needle.trim().is_empty() {
        println!("{}", "usage: /loop stop <id|name>".dimmed());
        return;
    }
    let target = match automations.list_automations().await {
        Ok(records) => match resolve(&records, needle.trim()) {
            Ok(record) => record.clone(),
            Err(err) => {
                println!("{} {err}", "✗".red());
                return;
            }
        },
        Err(err) => {
            println!("{} could not read the automations: {err}", "✗".red());
            return;
        }
    };
    match automations.delete_automation(&target.id).await {
        Ok(record) => println!(
            "{}",
            format!("stopped {} — its runs stay in the history", record.name).dimmed()
        ),
        Err(err) => println!("{} could not stop {}: {err}", "✗".red(), target.name),
    }
}

async fn set_paused(
    automations: &std::sync::Arc<dyn codesmith_agent_runtime::host_services::AutomationManagerHost>,
    needle: &str,
    verb: &str,
) {
    if needle.trim().is_empty() {
        println!("{}", format!("usage: /loop {verb} <id|name>").dimmed());
        return;
    }
    let target = match automations.list_automations().await {
        Ok(records) => match resolve(&records, needle.trim()) {
            Ok(record) => record.clone(),
            Err(err) => {
                println!("{} {err}", "✗".red());
                return;
            }
        },
        Err(err) => {
            println!("{} could not read the automations: {err}", "✗".red());
            return;
        }
    };
    let paused = verb.eq_ignore_ascii_case("pause");
    let result = if paused {
        automations.pause_automation(&target.id).await
    } else {
        automations.resume_automation(&target.id).await
    };
    match result {
        Ok(record) => println!(
            "{}",
            format!(
                "{} {} — next run {}",
                if paused { "paused" } else { "resumed" },
                record.name,
                when(record.next_run_at)
            )
            .dimmed()
        ),
        Err(err) => println!("{} could not {} {}: {err}", "✗".red(), verb, target.name),
    }
}

async fn run_now(
    state: &ReplState,
    automations: &std::sync::Arc<dyn codesmith_agent_runtime::host_services::AutomationManagerHost>,
    needle: &str,
) {
    if needle.trim().is_empty() {
        println!("{}", "usage: /loop run <id|name>".dimmed());
        return;
    }
    let Some(task_manager) = state.runtime_services.task_manager.clone() else {
        println!(
            "{}",
            "✗ this session has no task manager to run it on".red()
        );
        return;
    };
    let target = match automations.list_automations().await {
        Ok(records) => match resolve(&records, needle.trim()) {
            Ok(record) => record.clone(),
            Err(err) => {
                println!("{} {err}", "✗".red());
                return;
            }
        },
        Err(err) => {
            println!("{} could not read the automations: {err}", "✗".red());
            return;
        }
    };
    match automations.run_now(&target.id, &task_manager).await {
        Ok(run) => println!(
            "{}",
            format!(
                "queued {} now (task {}) — /tasks shows it running",
                target.name,
                run.task_id.as_deref().unwrap_or("pending")
            )
            .dimmed()
        ),
        Err(err) => println!("{} could not run {}: {err}", "✗".red(), target.name),
    }
}

fn first_line(text: &str) -> String {
    let line = text.lines().next().unwrap_or(text).trim();
    if line.chars().count() > 80 {
        line.chars().take(77).collect::<String>() + "…"
    } else {
        line.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Utc;

    #[test]
    fn hours_days_and_keywords_become_schedulable_rrules() {
        assert_eq!(interval_to_rrule("1h").expect("1h"), "FREQ=HOURLY");
        assert_eq!(
            interval_to_rrule("2h").expect("2h"),
            "FREQ=HOURLY;INTERVAL=2"
        );
        assert_eq!(
            interval_to_rrule("1d").expect("1d"),
            "FREQ=HOURLY;INTERVAL=24"
        );
        assert_eq!(
            interval_to_rrule("3d").expect("3d"),
            "FREQ=HOURLY;INTERVAL=72"
        );
        assert_eq!(interval_to_rrule("hourly").expect("hourly"), "FREQ=HOURLY");
        assert_eq!(
            interval_to_rrule("DAILY").expect("daily"),
            "FREQ=HOURLY;INTERVAL=24"
        );
        // Whatever this returns has to survive the parser that will read it.
        for interval in ["1h", "2h", "1d", "FREQ=HOURLY;INTERVAL=6"] {
            let rrule = interval_to_rrule(interval).expect(interval);
            AutomationSchedule::parse_rrule(&rrule)
                .expect("the scheduler must accept what we store");
        }
    }

    #[test]
    fn sub_hourly_intervals_are_refused_rather_than_rounded() {
        let err = interval_to_rrule("5m").expect_err("minutes");
        assert!(err.contains("one hour"), "{err}");
        assert!(interval_to_rrule("30s").is_err());
        assert!(
            interval_to_rrule("0h").is_err(),
            "a zero interval never stops"
        );
        assert!(interval_to_rrule("soon").is_err());
        assert!(interval_to_rrule("").is_err());
        // An RRULE the scheduler cannot parse must not be stored either.
        assert!(interval_to_rrule("FREQ=MINUTELY").is_err());
    }

    #[test]
    fn names_are_readable_and_bounded() {
        assert_eq!(
            name_for("check the deploy\nthen report"),
            "loop: check the deploy"
        );
        let long = name_for(&"x".repeat(200));
        assert!(long.chars().count() <= 60, "{long}");
    }

    fn record(id: &str, name: &str) -> AutomationRecord {
        AutomationRecord {
            schema_version: 1,
            id: id.to_string(),
            name: name.to_string(),
            prompt: "p".to_string(),
            rrule: "FREQ=HOURLY".to_string(),
            cwds: Vec::new(),
            status: AutomationStatus::Active,
            created_at: Utc::now(),
            updated_at: Utc::now(),
            next_run_at: None,
            last_run_at: None,
        }
    }

    #[test]
    fn a_target_is_addressed_by_id_prefix_or_name() {
        let records = vec![record("abcdef1234567890", "loop: check the deploy")];
        assert_eq!(
            resolve(&records, "abcdef1234567890").expect("full id").id,
            "abcdef1234567890"
        );
        assert_eq!(
            resolve(&records, "abcdef").expect("prefix").id,
            "abcdef1234567890"
        );
        assert_eq!(
            resolve(&records, "LOOP: CHECK THE DEPLOY")
                .expect("name")
                .id,
            "abcdef1234567890"
        );
    }

    #[test]
    fn an_ambiguous_or_unknown_target_says_which() {
        let records = vec![record("abcdef1111", "a"), record("abcdef2222", "b")];
        let err = resolve(&records, "abcdef").expect_err("ambiguous");
        assert!(err.contains("matches 2"), "{err}");
        let err = resolve(&records, "zzz").expect_err("unknown");
        assert!(err.contains("no loop"), "{err}");
        // Too short to be a prefix, and not a name.
        let err = resolve(&records, "abc").expect_err("too short");
        assert!(err.contains("no loop"), "{err}");
    }
}
