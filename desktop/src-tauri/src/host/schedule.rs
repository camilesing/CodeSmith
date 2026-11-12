//! `schedule/*` — the vendored client's task-manager page translated onto
//! CodeSmith automations (`/v1/automations`).
//!
//! The upstream schedule vocabulary is wider than automations: typed timing
//! (after/at/every/daily/weekly/cron/rrule), session delivery targets, and
//! delivery retention. Automations accept exactly two RRULE shapes —
//! `FREQ=HOURLY` (INTERVAL, BYDAY) and `FREQ=WEEKLY` (BYDAY, BYHOUR,
//! BYMINUTE), evaluated in the machine's local zone — so `timing_to_rrule`
//! compiles what fits (daily → every weekday, weekly, whole-hour intervals)
//! and fails loud on the rest instead of silently rescheduling it, and a
//! `new` delivery target is synthesized from the automation's working
//! directory so the catalog contract stays complete.
//!
//! Known limitations (the page states them in its module docs too):
//! - one-shot timings (after/at), sub-hour intervals, foreign time zones,
//!   and cron shapes outside the two grammars fail loud at create/edit;
//! - delivery retention is a fixed answer, not a configured limit;
//! - `schedule/run` triggers the automation immediately and reports success
//!   from the HTTP route, not from the run's outcome;
//! - the `expectedUpdatedAt` check is compare-then-PATCH (the runtime's PATCH
//!   takes no revision), so a concurrent write between the read and the
//!   patch can still be lost; the window is one loopback round trip.

use anyhow::{bail, Context, Result};
use chrono::Offset as _;
use serde_json::{json, Value};

use super::workspace::OpError;
use super::HostState;

/// The weekday codes RRULE uses, Monday-first to match the upstream input.
const WEEKDAY_CODES: [&str; 7] = ["MO", "TU", "WE", "TH", "FR", "SA", "SU"];

/// Compile an upstream `ScheduleTiming` into the one RRULE grammar
/// automations accept: `FREQ=HOURLY;INTERVAL=n[;BYDAY=…]` or
/// `FREQ=WEEKLY;BYDAY=…;BYHOUR=h;BYMINUTE=m` (machine-local clock). Timings
/// outside that grammar fail loud — silently rescheduling a "once in 2
/// hours" task as weekly would be a lie, not a fallback.
pub fn timing_to_rrule(timing: &Value) -> Result<String> {
    let kind = timing
        .get("kind")
        .and_then(Value::as_str)
        .unwrap_or_default();
    match kind {
        "rrule" => {
            let rule = timing
                .get("rrule")
                .and_then(Value::as_str)
                .context("rrule timing without a rule")?;
            validate_automation_rule(rule)
        }
        "every" => {
            let seconds = timing
                .get("every_seconds")
                .and_then(Value::as_i64)
                .context("every timing without every_seconds")?;
            if seconds <= 0 {
                bail!("every timing needs a positive interval");
            }
            if seconds % 3600 != 0 {
                bail!(
                    "CodeSmith automations run at whole-hour intervals or more;                      every {seconds}s is finer than that"
                );
            }
            Ok(format!("FREQ=HOURLY;INTERVAL={}", seconds / 3600))
        }
        "daily" => {
            let (hour, minute) = local_time(timing, "/daily")?;
            Ok(format!(
                "FREQ=WEEKLY;BYDAY={};BYHOUR={hour};BYMINUTE={minute}",
                WEEKDAY_CODES.join(",")
            ))
        }
        "weekly" => {
            let weekdays = timing
                .pointer("/weekly/weekdays")
                .and_then(Value::as_array)
                .context("weekly timing without weekdays")?;
            let days: Vec<&str> = weekdays
                .iter()
                .filter_map(|day| day.as_i64())
                .filter_map(|day| day.checked_sub(1))
                .filter(|index| (0..7).contains(index))
                .map(|index| WEEKDAY_CODES[index as usize])
                .collect();
            if days.is_empty() {
                bail!("weekly timing without a usable weekday");
            }
            let (hour, minute) = local_time(timing, "/weekly")?;
            Ok(format!(
                "FREQ=WEEKLY;BYDAY={};BYHOUR={hour};BYMINUTE={minute}",
                days.join(",")
            ))
        }
        "after" | "at" => bail!(
            "CodeSmith automations are recurring (hourly or weekly); a one-shot              timing has no equivalent"
        ),
        "cron" => cron_to_rule(timing),
        other => bail!("unsupported timing kind {other:?}"),
    }
}

/// The rule's clock fields, accepted only in the machine's own zone (the
/// scheduler evaluates BYHOUR/BYMINUTE locally; a foreign zone would drift
/// with its DST). Seconds truncate to the minute — the grammar has none.
fn local_time(timing: &Value, pointer: &str) -> Result<(u32, u32)> {
    let time = timing
        .pointer(&format!("{pointer}/time"))
        .and_then(Value::as_str)
        .unwrap_or("00:00");
    let zone = timing
        .pointer(&format!("{pointer}/time_zone"))
        .and_then(Value::as_str)
        .unwrap_or("");
    if !zone.is_empty() {
        let tz: chrono_tz::Tz = zone
            .parse()
            .with_context(|| format!("unknown time zone {zone:?}"))?;
        if !zone_matches_system(tz) {
            bail!(
                "CodeSmith automations schedule in the machine's local zone; \
                 {zone} keeps a different clock"
            );
        }
    }
    let (hour, minute, second) = parse_hms(time)?;
    if second != 0 {
        // Truncate rather than shift: 09:30:30 fires at 09:30.
        eprintln!(
            "codesmith desktop: schedule time {time:?} drops its seconds \
             (automations schedule at minute granularity)"
        );
    }
    Ok((hour, minute))
}

/// Whether a zone keeps the machine's wall clock right now — the scheduler
/// evaluates BYHOUR/BYMINUTE in `Local`, so a zone with the same current
/// offset fires at the requested time; a different offset would not.
fn zone_matches_system(tz: chrono_tz::Tz) -> bool {
    let now = chrono::Utc::now();
    now.with_timezone(&chrono::Local)
        .offset()
        .fix()
        .local_minus_utc()
        == now.with_timezone(&tz).offset().fix().local_minus_utc()
}

/// Split `HH:mm[:ss]` (optional fractional tail) into its parts.
fn parse_hms(text: &str) -> Result<(u32, u32, u32)> {
    let mut parts = text.split(':');
    let hour = parts
        .next()
        .and_then(|p| p.parse().ok())
        .context("bad hour")?;
    let minute = parts.next().and_then(|p| p.parse().ok()).unwrap_or(0);
    let second = parts
        .next()
        .and_then(|p| p.split('.').next().and_then(|s| s.parse().ok()))
        .unwrap_or(0);
    if hour > 23 || minute > 59 || second > 59 {
        bail!("time {text:?} out of range");
    }
    Ok((hour, minute, second))
}

/// Pass a raw `rrule` timing through only when it already speaks one of the
/// two automation grammars.
fn validate_automation_rule(rule: &str) -> Result<String> {
    let cleaned: Vec<String> = rule
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty() && !line.starts_with("DTSTART"))
        .map(|line| line.trim_start_matches("RRULE:").trim().to_string())
        .collect();
    let joined = cleaned.join(";");
    // Reuse the compiler's grammar by feeding it back through the same checks.
    let keys: Vec<String> = joined
        .split(';')
        .filter_map(|part| {
            part.split_once('=')
                .map(|(key, _)| key.to_ascii_uppercase())
        })
        .collect();
    let freq = joined
        .split(';')
        .find_map(|part| part.strip_prefix("FREQ=").map(str::to_ascii_uppercase))
        .unwrap_or_default();
    let freq = freq.trim().to_string();
    match freq.as_str() {
        "HOURLY" => {
            if keys
                .iter()
                .any(|key| !["FREQ", "INTERVAL", "BYDAY"].contains(&key.as_str()))
            {
                bail!("CodeSmith HOURLY automations allow only FREQ,INTERVAL,BYDAY");
            }
            Ok(joined)
        }
        "WEEKLY" => {
            if keys
                .iter()
                .any(|key| !["FREQ", "BYDAY", "BYHOUR", "BYMINUTE"].contains(&key.as_str()))
            {
                bail!("CodeSmith WEEKLY automations allow only FREQ,BYDAY,BYHOUR,BYMINUTE");
            }
            if !keys.iter().any(|key| key == "BYDAY") {
                bail!("CodeSmith WEEKLY automations require BYDAY");
            }
            Ok(joined)
        }
        other => {
            bail!("CodeSmith automations support FREQ=HOURLY and FREQ=WEEKLY only (got {other:?})")
        }
    }
}

/// Five-field Vixie cron → the automation grammar, when the shape fits: a
/// fixed clock on chosen weekdays becomes WEEKLY, a whole-hour cadence
/// becomes HOURLY, anything else fails loud.
fn cron_to_rule(timing: &Value) -> Result<String> {
    let expression = timing
        .pointer("/cron/expression")
        .and_then(Value::as_str)
        .context("cron timing without an expression")?;
    let fields: Vec<&str> = expression.split_whitespace().collect();
    if fields.len() != 5 {
        bail!("cron expression {expression:?} is not five fields");
    }
    let minute = CronField::parse(fields[0], 0, 59, "minute")?;
    let hour = CronField::parse(fields[1], 0, 23, "hour")?;
    let month_day = CronField::parse(fields[2], 1, 31, "day-of-month")?;
    let month = CronField::parse(fields[3], 1, 12, "month")?;
    let week_day = CronField::parse(fields[4], 0, 6, "day-of-week")?;
    if !month.is_star || !month_day.is_star {
        bail!("CodeSmith automations cannot restrict calendar days or months");
    }
    if !minute.is_star && !hour.is_star && week_day.is_star {
        // A fixed daily clock (possibly a list of hours) → WEEKLY, all days.
        return Ok(format!(
            "FREQ=WEEKLY;BYDAY={};BYHOUR={};BYMINUTE={}",
            WEEKDAY_CODES.join(","),
            hour.csv(),
            minute.csv(),
        ));
    }
    if minute.is_star && hour.is_star && !week_day.is_star {
        // Whole-hour cadence on chosen days → HOURLY + BYDAY.
        let days = week_day.iso_codes().join(",");
        return Ok(format!("FREQ=HOURLY;BYDAY={days}"));
    }
    if minute.values.as_slice() == [0] && hour.is_star && week_day.is_star {
        // `0 * * * *` — on the hour, every hour.
        return Ok("FREQ=HOURLY".into());
    }
    if minute.values.as_slice() == [0] && hour.is_star && !week_day.is_star {
        // `0 * * 1,5` — on the hour during chosen days.
        let days = week_day.iso_codes().join(",");
        return Ok(format!("FREQ=HOURLY;BYDAY={days}"));
    }
    if !minute.is_star && !hour.is_star && !week_day.is_star {
        return Ok(format!(
            "FREQ=WEEKLY;BYDAY={};BYHOUR={};BYMINUTE={}",
            week_day.iso_codes().join(","),
            hour.csv(),
            minute.csv(),
        ));
    }
    bail!("cron {expression:?} has no hourly/weekly automation equivalent")
}

/// One parsed cron field: the full set (`*`) or an explicit value list.
struct CronField {
    is_star: bool,
    values: Vec<u32>,
}

impl CronField {
    fn parse(text: &str, min: u32, max: u32, name: &str) -> Result<Self> {
        if text == "*" {
            return Ok(Self {
                is_star: true,
                values: Vec::new(),
            });
        }
        let mut values = Vec::new();
        for part in text.split(',') {
            let (range, step) = match part.split_once('/') {
                Some((range, step)) => (
                    range,
                    step.parse::<u32>()
                        .with_context(|| format!("bad {name} step"))?,
                ),
                None => (part, 1),
            };
            if step == 0 {
                bail!("cron {name} step of zero");
            }
            let (lo, hi) = if range == "*" {
                (min, max)
            } else if let Some((lo, hi)) = range.split_once('-') {
                (lo.parse()?, hi.parse()?)
            } else {
                let single: u32 = range
                    .parse()
                    .with_context(|| format!("bad {name} value {range:?}"))?;
                (single, single)
            };
            if lo < min || hi > max {
                bail!("cron {name} {text:?} out of {min}..={max}");
            }
            values.extend((lo..=hi).step_by(step as usize));
        }
        values.sort_unstable();
        values.dedup();
        if values.is_empty() {
            bail!("cron {name} {text:?} is empty");
        }
        Ok(Self {
            is_star: false,
            values,
        })
    }

    fn csv(&self) -> String {
        self.values
            .iter()
            .map(|value| value.to_string())
            .collect::<Vec<_>>()
            .join(",")
    }

    /// Cron day-of-week numbers (0/7=Sunday) → RRULE codes.
    fn iso_codes(&self) -> Vec<String> {
        self.values
            .iter()
            .map(|day| match day {
                0 | 7 => "SU".to_string(),
                n => WEEKDAY_CODES[(n - 1) as usize].to_string(),
            })
            .collect()
    }
}

/// Automation record → the catalog entry the client renders. The automation's
/// `cwds[0]` becomes the synthesized `new` delivery target; runs beyond the
/// record's `last_run_at` stay absent.
fn catalog_entry(automation: &Value, home: &str) -> Value {
    let id = automation.get("id").cloned().unwrap_or(Value::Null);
    let title = automation
        .get("name")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let prompt = automation
        .get("prompt")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let rrule = automation
        .get("rrule")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let next = automation.get("next_run_at").and_then(Value::as_str);
    let last = automation.get("last_run_at").and_then(Value::as_str);
    let status = match automation.get("status").and_then(Value::as_str) {
        Some("active") => "active",
        // Automations know active/paused; an automation with no next
        // occurrence left reads as ended.
        _ if next.is_none() && last.is_some() => "ended",
        _ => "paused",
    };
    let cwd = automation
        .get("cwds")
        .and_then(Value::as_array)
        .and_then(|dirs| dirs.first())
        .and_then(Value::as_str)
        .unwrap_or(home);
    let mut entry = json!({
        "kind": "rrule",
        "id": id,
        "title": title,
        "prompt": prompt,
        "rrule": rrule,
        "scheduledAt": next.or(last).unwrap_or_default(),
        "target": { "kind": "new", "create": { "cwd": cwd } },
        "involvedSessionIds": [],
        "status": status,
    });
    let obj = entry.as_object_mut().unwrap();
    if let Some(created) = automation.get("created_at") {
        obj.insert("createdAt".into(), created.clone());
    }
    if let Some(updated) = automation.get("updated_at") {
        obj.insert("updatedAt".into(), updated.clone());
    }
    entry
}

/// `schedule/catalog` — the whole catalog as one bare array (the client's
/// `list()` reads the result straight into its records). `active` filters the
/// runtime's own automations list.
pub async fn catalog(host: &HostState, request: &Value) -> Result<Value> {
    let active = request.get("active").and_then(Value::as_bool);
    let query = match active {
        Some(true) => "?status=active",
        _ => "",
    };
    let automations: Vec<Value> = host
        .backend
        .get(&format!("/v1/automations{query}"))
        .await
        .context("list automations")?;
    let home = host.cwd.to_string_lossy().to_string();
    let entries: Vec<Value> = automations
        .iter()
        .map(|automation| catalog_entry(automation, &home))
        .collect();
    Ok(Value::Array(entries))
}

/// `schedule/create` — compile the typed timing and POST the automation.
pub async fn create(host: &HostState, request: &Value) -> Result<Value> {
    let request = request.get("request").unwrap_or(request);
    let title = request
        .get("title")
        .and_then(Value::as_str)
        .context("create without title")?;
    let prompt = request
        .get("prompt")
        .and_then(Value::as_str)
        .context("create without prompt")?;
    let timing = request.get("timing").context("create without timing")?;
    let rrule = timing_to_rrule(timing)?;
    // A session delivery target has no automation equivalent: runs always
    // spawn in a working directory, so the requested create cwd (if any) wins.
    let cwd = request
        .pointer("/target/create/cwd")
        .and_then(Value::as_str)
        .map(str::to_string);
    let mut body = json!({ "name": title, "prompt": prompt, "rrule": rrule });
    if let Some(cwd) = cwd {
        body["cwds"] = json!([cwd]);
    }
    let automation: Value = host
        .backend
        .post("/v1/automations", &body)
        .await
        .context("create automation")?;
    let home = host.cwd.to_string_lossy().to_string();
    Ok(catalog_entry(&automation, &home))
}

/// A stale `expectedUpdatedAt`: the task changed since the editor read it.
/// Typed so `edit` can answer the wire's `schedule_conflict` code instead of
/// a generic arguments error.
#[derive(Debug)]
struct StaleRevision;

impl std::fmt::Display for StaleRevision {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "the task changed since it was read")
    }
}

impl std::error::Error for StaleRevision {}

/// `schedule/edit` — PATCH the mutable fields; a timing edit recompiles.
/// `expectedUpdatedAt` is enforced here (the runtime's PATCH ignores it):
/// the stored `updated_at` must equal the revision the editor read, and a
/// mismatch answers `schedule_conflict` so the client can offer to review
/// its edits instead of overwriting a newer task.
pub async fn edit(host: &HostState, request: &Value) -> Result<Value, OpError> {
    match edit_inner(host, request).await {
        Ok(value) => Ok(value),
        Err(err) if err.downcast_ref::<StaleRevision>().is_some() => Err(OpError {
            code: "schedule_conflict",
            message: format!("schedule/edit: {err}"),
        }),
        Err(err) => Err(OpError {
            code: "gateway/arguments-invalid",
            message: format!("schedule/edit: {err:#}"),
        }),
    }
}

async fn edit_inner(host: &HostState, request: &Value) -> Result<Value> {
    let request = request.get("request").unwrap_or(request);
    let id = request
        .get("id")
        .and_then(Value::as_str)
        .context("edit without id")?;
    let mut body = json!({});
    let obj = body.as_object_mut().unwrap();
    if let Some(title) = request.get("title").and_then(Value::as_str) {
        obj.insert("name".into(), json!(title));
    }
    if let Some(prompt) = request.get("prompt").and_then(Value::as_str) {
        obj.insert("prompt".into(), json!(prompt));
    }
    if let Some(timing) = request.get("timing") {
        let rrule = timing_to_rrule(timing)?;
        obj.insert("rrule".into(), json!(rrule));
    }
    if let Some(active) = request.get("active").and_then(Value::as_bool) {
        obj.insert(
            "status".into(),
            json!(if active { "active" } else { "paused" }),
        );
    }
    if obj.is_empty() {
        bail!("edit without a change");
    }
    if let Some(expected) = request.get("expectedUpdatedAt") {
        let automations: Vec<Value> = host
            .backend
            .get("/v1/automations")
            .await
            .context("read automations for the revision check")?;
        let current = automations
            .iter()
            .find(|automation| automation.get("id").and_then(Value::as_str) == Some(id))
            .and_then(|automation| automation.get("updated_at"))
            .cloned()
            .unwrap_or(Value::Null);
        if &current != expected {
            return Err(anyhow::Error::new(StaleRevision));
        }
    }
    let automation: Value = host
        .backend
        .patch(&format!("/v1/automations/{id}"), &body)
        .await
        .with_context(|| format!("edit automation {id}"))?;
    let home = host.cwd.to_string_lossy().to_string();
    Ok(catalog_entry(&automation, &home))
}

/// `schedule/run` — trigger one occurrence now.
pub async fn run(host: &HostState, request: &Value) -> Result<Value> {
    let request = request.get("request").unwrap_or(request);
    let id = request
        .get("id")
        .and_then(Value::as_str)
        .context("run without id")?;
    let _: Value = host
        .backend
        .post(&format!("/v1/automations/{id}/run"), &json!({}))
        .await
        .with_context(|| format!("run automation {id}"))?;
    Ok(json!({ "id": id, "triggered": true }))
}

/// `schedule/history` — the automation's run records as delivery receipts.
pub async fn history(host: &HostState, request: &Value) -> Result<Value> {
    let request = request.get("request").unwrap_or(request);
    let id = request
        .get("id")
        .and_then(Value::as_str)
        .context("history without id")?;
    let runs: Vec<Value> = host
        .backend
        .get(&format!("/v1/automations/{id}/runs?limit=100"))
        .await
        .with_context(|| format!("runs of automation {id}"))?;
    let records: Vec<Value> = runs
        .iter()
        .map(|run| {
            json!({
                "scheduledAt": run.get("scheduled_for").cloned().unwrap_or_default(),
                "deliveredAt": run.get("ended_at").or_else(|| run.get("started_at")).cloned()
                    .unwrap_or_else(|| run.get("created_at").cloned().unwrap_or_default()),
                "messageId": run.get("id").cloned().unwrap_or_default(),
                "targetSessionId": "",
                "prompt": run.get("prompt").cloned().unwrap_or_default(),
            })
        })
        .collect();
    Ok(json!({
        "id": id,
        "records": records,
        "earlierRecordsUnavailable": false,
        "earlierRecordsPruned": false,
        "retention": { "days": 30, "records": 100 },
    }))
}

/// `schedule/deleteInactive` — delete every paused/ended task; the answer is
/// the deleted id set.
pub async fn delete_inactive(host: &HostState, _request: &Value) -> Result<Value> {
    let automations: Vec<Value> = host
        .backend
        .get("/v1/automations")
        .await
        .context("list automations")?;
    let home = host.cwd.to_string_lossy().to_string();
    let mut deleted = Vec::new();
    for automation in &automations {
        let entry = catalog_entry(automation, &home);
        let status = entry
            .get("status")
            .and_then(Value::as_str)
            .unwrap_or("active");
        if status == "active" {
            continue;
        }
        let id = automation
            .get("id")
            .and_then(Value::as_str)
            .unwrap_or_default();
        let _: Value = host
            .backend
            .delete(&format!("/v1/automations/{id}"))
            .await
            .with_context(|| format!("delete automation {id}"))?;
        deleted.push(json!(id));
    }
    Ok(json!({ "deleted": deleted }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn daily_and_weekly_compile_to_the_automation_grammar() {
        let daily = timing_to_rrule(&json!({
            "kind": "daily", "daily": { "time": "09:30" }
        }))
        .unwrap();
        assert_eq!(
            daily,
            "FREQ=WEEKLY;BYDAY=MO,TU,WE,TH,FR,SA,SU;BYHOUR=9;BYMINUTE=30"
        );
        let weekly = timing_to_rrule(&json!({
            "kind": "weekly",
            "weekly": { "time": "00:15", "weekdays": [1, 5] }
        }))
        .unwrap();
        assert_eq!(weekly, "FREQ=WEEKLY;BYDAY=MO,FR;BYHOUR=0;BYMINUTE=15");
        // A foreign zone with a different clock is refused, not rescheduled.
        let wrong_zone = timing_to_rrule(&json!({
            "kind": "daily",
            "daily": { "time": "09:30", "time_zone": zone_offset_apart_from_local() }
        }));
        assert!(wrong_zone.is_err(), "a foreign clock must fail loud");
    }

    /// A zone whose current offset differs from the machine's; empty on
    /// machines where every fixed-offset zone happens to share the local one.
    fn zone_offset_apart_from_local() -> String {
        let local = chrono::Local::now().offset().fix().local_minus_utc() / 3600;
        [
            "Etc/GMT-14",
            "Etc/GMT-12",
            "Etc/GMT-10",
            "Etc/GMT-8",
            "Etc/GMT-6",
            "Etc/GMT-4",
            "Etc/GMT-2",
            "Etc/GMT",
            "Etc/GMT+2",
            "Etc/GMT+4",
            "Etc/GMT+6",
            "Etc/GMT+8",
            "Etc/GMT+10",
            "Etc/GMT+12",
        ]
        .iter()
        .find(|name| {
            name.parse::<chrono_tz::Tz>()
                .map(|tz| {
                    chrono::Utc::now()
                        .with_timezone(&tz)
                        .offset()
                        .fix()
                        .local_minus_utc()
                        / 3600
                        != local
                })
                .unwrap_or(false)
        })
        .map(|name| name.to_string())
        .unwrap_or_default()
    }

    #[test]
    fn every_maps_only_whole_hours_and_one_shots_fail_loud() {
        let hourly = timing_to_rrule(&json!({ "kind": "every", "every_seconds": 7200 })).unwrap();
        assert_eq!(hourly, "FREQ=HOURLY;INTERVAL=2");
        let minutes = timing_to_rrule(&json!({ "kind": "every", "every_seconds": 90 }));
        assert!(minutes.is_err(), "sub-hour intervals have no automation");
        let after = timing_to_rrule(&json!({ "kind": "after", "after_seconds": 60 }));
        assert!(after.is_err(), "one-shot timings have no automation");
        let at = timing_to_rrule(&json!({ "kind": "at", "at": "2026-12-01T09:00:00Z" }));
        assert!(at.is_err(), "absolute one-shot timings have no automation");
    }

    #[test]
    fn cron_maps_daily_clocks_and_hourly_cadence_only() {
        let daily_clock = timing_to_rrule(&json!({
            "kind": "cron", "cron": { "expression": "30 9 * * *" }
        }))
        .unwrap();
        assert_eq!(
            daily_clock,
            "FREQ=WEEKLY;BYDAY=MO,TU,WE,TH,FR,SA,SU;BYHOUR=9;BYMINUTE=30"
        );
        let on_hours = timing_to_rrule(&json!({
            "kind": "cron", "cron": { "expression": "0 * * * 1,5" }
        }))
        .unwrap();
        assert_eq!(on_hours, "FREQ=HOURLY;BYDAY=MO,FR");
        let day_bound = timing_to_rrule(&json!({
            "kind": "cron", "cron": { "expression": "0 0 13 * 5" }
        }));
        assert!(
            day_bound.is_err(),
            "calendar-day restriction is unsupported"
        );
        let ten_past = timing_to_rrule(&json!({
            "kind": "cron", "cron": { "expression": "10 * * * *" }
        }));
        assert!(ten_past.is_err(), "an hourly rule cannot carry minutes");
        let named = timing_to_rrule(&json!({
            "kind": "cron", "cron": { "expression": "@daily" }
        }));
        assert!(named.is_err(), "named cron expressions fail loud");
    }

    #[test]
    fn rrule_passthrough_accepts_only_the_automation_grammar() {
        let weekly = timing_to_rrule(&json!({
            "kind": "rrule", "rrule": "RRULE:FREQ=WEEKLY;BYDAY=MO;BYHOUR=9;BYMINUTE=30"
        }))
        .unwrap();
        assert_eq!(weekly, "FREQ=WEEKLY;BYDAY=MO;BYHOUR=9;BYMINUTE=30");
        let hourly = timing_to_rrule(&json!({
            "kind": "rrule", "rrule": "FREQ=HOURLY;INTERVAL=3"
        }))
        .unwrap();
        assert_eq!(hourly, "FREQ=HOURLY;INTERVAL=3");
        let daily = timing_to_rrule(&json!({ "kind": "rrule", "rrule": "FREQ=DAILY" }));
        assert!(daily.is_err(), "DAILY is outside the automation grammar");
        let with_seconds = timing_to_rrule(&json!({
            "kind": "rrule", "rrule": "FREQ=WEEKLY;BYDAY=MO;BYSECOND=30"
        }));
        assert!(with_seconds.is_err(), "BYSECOND is outside the grammar");
    }

    #[test]
    fn catalog_entry_maps_status_and_target() {
        let home = "/Users/x/proj";
        let active = catalog_entry(
            &json!({
                "id": "a1", "name": "Tidy", "prompt": "run tidy", "rrule": "RRULE:FREQ=DAILY",
                "cwds": ["/Users/x/proj"], "status": "active",
                "created_at": "2026-11-01T00:00:00Z", "updated_at": "2026-11-02T00:00:00Z",
                "next_run_at": "2026-11-08T00:00:00Z",
            }),
            home,
        );
        assert_eq!(active["status"], "active");
        assert_eq!(active["kind"], "rrule");
        assert_eq!(active["target"]["kind"], "new");
        assert_eq!(active["target"]["create"]["cwd"], "/Users/x/proj");
        assert_eq!(active["scheduledAt"], "2026-11-08T00:00:00Z");

        let drained = catalog_entry(
            &json!({
                "id": "a2", "name": "Once", "prompt": "once", "rrule": "RRULE:FREQ=DAILY;COUNT=1",
                "cwds": [], "status": "paused",
                "created_at": "2026-11-01T00:00:00Z", "updated_at": "2026-11-02T00:00:00Z",
                "last_run_at": "2026-11-02T09:00:00Z",
            }),
            home,
        );
        assert_eq!(drained["status"], "ended", "no next occurrence left");
    }
}
