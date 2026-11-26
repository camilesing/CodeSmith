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
//! - the catalog's `rrule` carries a synthesized `DTSTART;TZID=<machine zone>`
//!   line so the page reads the zone and edits the rule as an RRULE; the
//!   scheduler ignores `DTSTART` and evaluates the rule in the machine's
//!   zone, and a weekly rule keeps its clock in BYHOUR/BYMINUTE, which the
//!   page edits as Custom, never as its guided Weekly;
//! - delivery retention is a fixed answer, not a configured limit;
//! - `schedule/run` triggers the automation immediately and reports success
//!   from the HTTP route, not from the run's outcome;
//! - the `expectedUpdatedAt` check is compare-then-PATCH (the runtime's PATCH
//!   takes no revision), so a concurrent write between the read and the
//!   patch can still be lost; the window is one loopback round trip.

use std::sync::OnceLock;

use anyhow::{bail, Context, Result};
use chrono::{Offset as _, Timelike};
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
                    "CodeSmith automations run at whole-hour intervals or more; \
                     every {seconds}s is finer than that"
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
            "CodeSmith automations are recurring (hourly or weekly); a one-shot \
             timing has no equivalent"
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
    check_dtstart_zone(rule)?;
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
            if keys.iter().any(|key| {
                !["FREQ", "INTERVAL", "BYDAY", "BYHOUR", "BYMINUTE"].contains(&key.as_str())
            }) {
                bail!(
                    "CodeSmith WEEKLY automations allow only FREQ,INTERVAL,BYDAY,BYHOUR,BYMINUTE"
                );
            }
            let field = |name: &str| {
                joined
                    .split(';')
                    .find_map(|part| part.strip_prefix(&format!("{name}=")))
            };
            let days = field("BYDAY").context("CodeSmith WEEKLY automations require BYDAY")?;
            // The task page's guided Weekly/Weekdays presets (found failing in
            // the desktop) post an INTERVAL and keep their clock on the
            // DTSTART stamp alone. A step of one is every week and is
            // dropped; another step has no equivalent.
            match field("INTERVAL") {
                None | Some("1") => {}
                Some(step) => bail!(
                    "CodeSmith WEEKLY automations run every week; every {step} weeks has no equivalent"
                ),
            }
            match (field("BYHOUR"), field("BYMINUTE")) {
                (Some(hour), Some(minute)) => Ok(format!(
                    "FREQ=WEEKLY;BYDAY={days};BYHOUR={hour};BYMINUTE={minute}"
                )),
                (None, None) => {
                    let (hour, minute) = dtstart_clock(rule)?;
                    Ok(format!("FREQ=WEEKLY;BYDAY={days};BYHOUR={hour};BYMINUTE={minute}"))
                }
                _ => bail!("CodeSmith WEEKLY automations need both BYHOUR and BYMINUTE, or a DTSTART clock"),
            }
        }
        // The task page's "every day" arrives as FREQ=DAILY;INTERVAL=1 at a
        // fixed clock — the same schedule as every weekday at that clock.
        // Any other daily shape (a step, a day filter) has no equivalent.
        "DAILY" => {
            if keys
                .iter()
                .any(|key| !["FREQ", "INTERVAL", "BYHOUR", "BYMINUTE"].contains(&key.as_str()))
            {
                bail!("CodeSmith DAILY automations allow only FREQ,INTERVAL,BYHOUR,BYMINUTE");
            }
            let field = |name: &str| {
                joined
                    .split(';')
                    .find_map(|part| part.strip_prefix(&format!("{name}=")))
            };
            match (field("INTERVAL"), field("BYHOUR"), field("BYMINUTE")) {
                (None | Some("1"), Some(hour), Some(minute)) => Ok(format!(
                    "FREQ=WEEKLY;BYDAY={};BYHOUR={hour};BYMINUTE={minute}",
                    WEEKDAY_CODES.join(",")
                )),
                _ => {
                    bail!("CodeSmith DAILY automations need INTERVAL=1 and a BYHOUR/BYMINUTE clock")
                }
            }
        }
        other => {
            bail!("CodeSmith automations support FREQ=HOURLY and FREQ=WEEKLY only (got {other:?})")
        }
    }
}

/// The clock of a rule's `DTSTART…:YYYYMMDDTHHMMSS[Z]` stamp, where the
/// task page's guided Weekly preset keeps it (its RRULE carries no
/// BYHOUR/BYMINUTE). Seconds truncate to the minute, as the grammar has none.
fn dtstart_clock(rule: &str) -> Result<(u32, u32)> {
    let time = rule
        .lines()
        .map(str::trim)
        .find(|line| line.starts_with("DTSTART"))
        .and_then(|line| line.rsplit_once(':'))
        .map(|(_, stamp)| stamp.trim_end_matches('Z'))
        .and_then(|stamp| stamp.split_once('T'))
        .map(|(_, time)| time)
        .filter(|time| time.len() == 6 && time.bytes().all(|byte| byte.is_ascii_digit()))
        .context("CodeSmith WEEKLY automations need a clock: BYHOUR/BYMINUTE or a DTSTART stamp")?;
    let (hour, minute, _) = parse_hms(&format!("{}:{}:{}", &time[..2], &time[2..4], &time[4..]))?;
    Ok((hour, minute))
}

/// A `DTSTART;TZID=<zone>:…` line must keep the machine's clock, and a `…Z`
/// stamp is UTC — the scheduler reads BYHOUR/BYMINUTE locally, so this is the
/// `rrule` kind's half of the contract `local_time` holds for the others.
fn check_dtstart_zone(rule: &str) -> Result<()> {
    let Some(dtstart) = rule
        .lines()
        .map(str::trim)
        .find(|line| line.starts_with("DTSTART"))
    else {
        return Ok(());
    };
    let zone = dtstart
        .split([';', ':'])
        .find_map(|part| part.strip_prefix("TZID="))
        .map(|tzid| {
            tzid.parse::<chrono_tz::Tz>()
                .with_context(|| format!("unknown time zone {tzid:?}"))
        })
        .transpose()?
        .or_else(|| dtstart.ends_with('Z').then_some(chrono_tz::Tz::UTC));
    match zone {
        Some(zone) if !zone_matches_system(zone) => bail!(
            "CodeSmith automations schedule in the machine's local zone; \
             {dtstart} keeps a different clock"
        ),
        _ => Ok(()),
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

/// The machine's IANA zone, resolved once. `None` when the system names no
/// zone; the catalog then carries the bare rule.
fn machine_zone() -> Option<&'static str> {
    static ZONE: OnceLock<Option<String>> = OnceLock::new();
    ZONE.get_or_init(|| iana_time_zone::get_timezone().ok())
        .as_deref()
}

/// The `DTSTART;TZID=…` stamp of one automation's catalog rule: its creation
/// date in `zone`, at the rule's own clock so the page's guided editor
/// starts from the time the task really runs. A rule without a clock (an
/// hourly one) keeps the creation instant.
fn local_stamp(rule: &str, created_at: &str, zone: &str) -> Option<String> {
    let tz: chrono_tz::Tz = zone.parse().ok()?;
    let created = chrono::DateTime::parse_from_rfc3339(created_at)
        .ok()?
        .with_timezone(&tz);
    let field = |name: &str| {
        rule.split(';').find_map(|part| {
            let (key, value) = part.trim_start_matches("RRULE:").split_once('=')?;
            key.eq_ignore_ascii_case(name)
                .then(|| value.trim().parse().ok())
                .flatten()
        })
    };
    let start = match (field("BYHOUR"), field("BYMINUTE")) {
        (Some(hour), Some(minute)) => created
            .with_hour(hour)?
            .with_minute(minute)?
            .with_second(0)?,
        _ => created,
    };
    Some(start.format("%Y%m%dT%H%M%S").to_string())
}

/// The catalog entry's `rrule` in the shape the page parses — one
/// `DTSTART;TZID=<machine zone>` line and one `RRULE:` line. The page reads
/// a rule without the `DTSTART` as UTC-zoned and takes edited rule text for
/// a cron expression, so a task's rule could not be edited at all; the
/// scheduler evaluates the rule in the machine's zone, which the line names
/// (and ignores on the way back). A machine that names no zone keeps the
/// bare rule.
fn catalog_rrule(rule: &str, created_at: &str, zone: Option<&str>) -> String {
    if rule.is_empty() {
        return String::new();
    }
    match zone.map(|zone| (zone, local_stamp(rule, created_at, zone))) {
        Some((zone, Some(stamp))) => format!("DTSTART;TZID={zone}:{stamp}\nRRULE:{rule}"),
        _ => rule.to_string(),
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
    let rrule = catalog_rrule(
        rrule,
        automation
            .get("created_at")
            .and_then(Value::as_str)
            .unwrap_or_default(),
        machine_zone(),
    );
    let next = automation.get("next_run_at").and_then(Value::as_str);
    let last = automation.get("last_run_at").and_then(Value::as_str);
    let status = match automation.get("status").and_then(Value::as_str) {
        // Automations know active/paused; an active one with no next
        // occurrence left has run out.
        Some("active") if next.is_none() && last.is_some() => "ended",
        Some("active") => "active",
        // Pausing clears `next_run_at` and keeps `last_run_at`, so a paused
        // automation that has run still reads as paused.
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

/// `schedule/edit` — the task page's one mutation verb. `action: "update"`
/// applies `patch` (title / prompt / timing / target / active) as one atomic
/// change — a timing edit recompiles, `active` is the automation's status —
/// and `action: "delete"` removes the task and answers `{ id, deleted }`.
/// Either way a result without a `code` member reads as saved.
///
/// `expectedUpdatedAt` is enforced here (the runtime's PATCH ignores it):
/// the stored `updated_at` must equal the revision the editor read, and a
/// mismatch answers `schedule_conflict` so the client can offer to review
/// its edits instead of overwriting a newer task. `null` — the editor read
/// no revision — skips the check.
///
/// Not applied: a `session` delivery target (automations always run in a
/// working directory), so an edit whose only change is that target is an
/// edit without a change; and a task the runtime no longer holds answers
/// its 404 as an arguments error, not `schedule_not_found`.
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
    let delete = match request.get("action").and_then(Value::as_str) {
        None | Some("update") => false,
        Some("delete") => true,
        Some(other) => bail!("edit with unknown action {other:?}"),
    };
    // The page nests its change in `patch`; a caller may also send the
    // fields bare, so the request itself is the fallback.
    let changes = request
        .get("patch")
        .filter(|patch| patch.is_object())
        .unwrap_or(request);
    let mut body = json!({});
    let obj = body.as_object_mut().unwrap();
    if let Some(title) = changes.get("title").and_then(Value::as_str) {
        obj.insert("name".into(), json!(title));
    }
    if let Some(prompt) = changes.get("prompt").and_then(Value::as_str) {
        obj.insert("prompt".into(), json!(prompt));
    }
    if let Some(timing) = changes.get("timing") {
        let rrule = timing_to_rrule(timing)?;
        obj.insert("rrule".into(), json!(rrule));
    }
    // A delivery target's created-session cwd is the automation's run
    // directory, the same mapping `create` makes.
    if let Some(cwd) = changes
        .pointer("/target/create/cwd")
        .and_then(Value::as_str)
    {
        obj.insert("cwds".into(), json!([cwd]));
    }
    if let Some(active) = changes.get("active").and_then(Value::as_bool) {
        obj.insert(
            "status".into(),
            json!(if active { "active" } else { "paused" }),
        );
    }
    if !delete && obj.is_empty() {
        bail!("edit without a change");
    }
    if let Some(expected) = request
        .get("expectedUpdatedAt")
        .filter(|expected| !expected.is_null())
    {
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
    if delete {
        let _: Value = host
            .backend
            .delete(&format!("/v1/automations/{id}"))
            .await
            .with_context(|| format!("delete automation {id}"))?;
        return Ok(json!({ "id": id, "deleted": true }));
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
        // The task page's "every day" (found failing in the desktop): a daily
        // step of one at a fixed clock is every weekday at that clock.
        let daily = timing_to_rrule(&json!({
            "kind": "rrule", "rrule": "RRULE:FREQ=DAILY;INTERVAL=1;BYHOUR=9;BYMINUTE=38"
        }))
        .unwrap();
        assert_eq!(
            daily,
            "FREQ=WEEKLY;BYDAY=MO,TU,WE,TH,FR,SA,SU;BYHOUR=9;BYMINUTE=38"
        );
        let stepped = timing_to_rrule(&json!({
            "kind": "rrule", "rrule": "FREQ=DAILY;INTERVAL=2;BYHOUR=9;BYMINUTE=38"
        }));
        assert!(stepped.is_err(), "every other day has no equivalent");
        let daily = timing_to_rrule(&json!({ "kind": "rrule", "rrule": "FREQ=DAILY" }));
        assert!(daily.is_err(), "DAILY is outside the automation grammar");
        // The task page's guided Weekly/Weekdays presets (found failing in
        // the desktop): the clock rides the DTSTART stamp and the rule posts
        // INTERVAL. A step of one compiles; another one has no equivalent.
        let preset = timing_to_rrule(&json!({
            "kind": "rrule",
            "rrule": "DTSTART:20261127T093045\nRRULE:FREQ=WEEKLY;INTERVAL=1;BYDAY=MO,TU,WE,TH,FR",
        }))
        .unwrap();
        assert_eq!(
            preset,
            "FREQ=WEEKLY;BYDAY=MO,TU,WE,TH,FR;BYHOUR=9;BYMINUTE=30"
        );
        let stepped = timing_to_rrule(&json!({
            "kind": "rrule",
            "rrule": "DTSTART:20261127T093000\nRRULE:FREQ=WEEKLY;INTERVAL=2;BYDAY=MO",
        }));
        assert!(stepped.is_err(), "every other week has no equivalent");
        let clockless = timing_to_rrule(&json!({
            "kind": "rrule", "rrule": "RRULE:FREQ=WEEKLY;BYDAY=MO",
        }));
        assert!(
            clockless.is_err(),
            "a weekly rule without a clock must not reach the runtime"
        );
        let with_seconds = timing_to_rrule(&json!({
            "kind": "rrule", "rrule": "FREQ=WEEKLY;BYDAY=MO;BYSECOND=30"
        }));
        assert!(with_seconds.is_err(), "BYSECOND is outside the grammar");
    }

    /// Found in the desktop: the task page sends `DTSTART;TZID=…` with its
    /// rrule and the bridge dropped the line unread, so a timezone picked on
    /// the page was silently scheduled as local time.
    #[test]
    fn rrule_dtstart_zone_must_keep_the_local_clock() {
        let rule = |dtstart: &str| {
            timing_to_rrule(&json!({
                "kind": "rrule",
                "rrule": format!("{dtstart}\nRRULE:FREQ=WEEKLY;BYDAY=MO;BYHOUR=9;BYMINUTE=38"),
            }))
        };
        let refused = rule(&format!(
            "DTSTART;TZID={}:20261124T093800",
            zone_offset_apart_from_local()
        ))
        .expect_err("a foreign clock must fail loud");
        assert!(refused.to_string().contains("local zone"), "{refused:#}");
        assert!(
            rule("DTSTART;TZID=Mars/Olympus:20261124T093800").is_err(),
            "an unknown zone must fail loud"
        );
        assert_eq!(
            rule("DTSTART:20261124T093800").unwrap(),
            "FREQ=WEEKLY;BYDAY=MO;BYHOUR=9;BYMINUTE=38",
            "a floating local stamp passes"
        );
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

        // Found in the desktop: a task paused after its first run read as
        // ended, and the page disables Resume for an ended task. Pausing
        // clears `next_run_at` and keeps `last_run_at`.
        let paused = catalog_entry(
            &json!({
                "id": "a2", "name": "Once", "prompt": "once", "rrule": "RRULE:FREQ=DAILY;COUNT=1",
                "cwds": [], "status": "paused",
                "created_at": "2026-11-01T00:00:00Z", "updated_at": "2026-11-02T00:00:00Z",
                "last_run_at": "2026-11-02T09:00:00Z",
            }),
            home,
        );
        assert_eq!(paused["status"], "paused", "a paused task that has run");

        let drained = catalog_entry(
            &json!({
                "id": "a2", "name": "Once", "prompt": "once", "rrule": "RRULE:FREQ=DAILY;COUNT=1",
                "cwds": [], "status": "active",
                "created_at": "2026-11-01T00:00:00Z", "updated_at": "2026-11-02T00:00:00Z",
                "last_run_at": "2026-11-02T09:00:00Z",
            }),
            home,
        );
        assert_eq!(drained["status"], "ended", "no next occurrence left");
    }

    /// Found in the desktop: the catalog carried a bare `RRULE`, which the
    /// page reads as UTC-zoned and replaces, once edited, with a cron
    /// expression — a task's zone showed `UTC+00:00` and its rule could not
    /// be edited. The page parses one DTSTART line and one RRULE line.
    #[test]
    fn catalog_rrule_names_the_zone_on_the_rules_clock() {
        let weekly = "FREQ=WEEKLY;BYDAY=MO,TU,WE,TH,FR,SA,SU;BYHOUR=9;BYMINUTE=50";
        assert_eq!(
            catalog_rrule(weekly, "2026-11-24T00:50:40.236558Z", Some("Asia/Shanghai")),
            format!("DTSTART;TZID=Asia/Shanghai:20261124T095000\nRRULE:{weekly}"),
            "the creation date in the zone, at the rule's own clock"
        );
        // An hourly rule has no clock: the creation instant anchors it.
        assert_eq!(
            catalog_rrule(
                "FREQ=HOURLY;INTERVAL=2",
                "2026-11-24T00:50:40.236558Z",
                Some("Asia/Shanghai")
            ),
            "DTSTART;TZID=Asia/Shanghai:20261124T085040\nRRULE:FREQ=HOURLY;INTERVAL=2"
        );
        // No resolvable zone, or no creation instant to date: the bare rule.
        assert_eq!(catalog_rrule(weekly, "2026-11-24T00:50:40Z", None), weekly);
        assert_eq!(catalog_rrule(weekly, "", Some("Asia/Shanghai")), weekly);
        assert_eq!(
            catalog_rrule(weekly, "2026-11-24T00:50:40Z", Some("Mars/Olympus")),
            weekly
        );
    }

    /// The page sends an edited rule back as the entry's own text; with the
    /// machine zone on its DTSTART line that text compiles to the stored rule.
    #[test]
    fn catalog_rrule_compiles_back_to_the_stored_rule() {
        let rule = "FREQ=WEEKLY;BYDAY=MO;BYHOUR=9;BYMINUTE=30";
        let entry = catalog_entry(
            &json!({
                "id": "a1", "name": "Tidy", "prompt": "run tidy", "rrule": rule, "cwds": [],
                "status": "active",
                "created_at": "2026-11-24T00:50:40Z", "updated_at": "2026-11-24T00:50:40Z",
                "next_run_at": "2026-11-30T09:30:00Z",
            }),
            "/Users/x/proj",
        );
        let rrule = entry["rrule"].as_str().expect("entry rrule");
        match machine_zone() {
            Some(zone) => assert!(
                rrule.starts_with(&format!("DTSTART;TZID={zone}:")),
                "the DTSTART line names the machine zone: {rrule}"
            ),
            None => assert_eq!(rrule, rule, "no zone: the bare rule"),
        }
        assert_eq!(
            timing_to_rrule(&json!({
                "kind": "rrule", "rrule": rrule.replace("BYMINUTE=30", "BYMINUTE=45"),
            }))
            .unwrap(),
            "FREQ=WEEKLY;BYDAY=MO;BYHOUR=9;BYMINUTE=45"
        );
    }
}
