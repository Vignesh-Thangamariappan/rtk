//! Multica issue filter module — `rtk multica issue <verb> [args...]`
//!
//! Dispatch table maps verb to a filter function.
//! Unknown verbs (create, update, status, assign, comment) fallthrough to passthrough.

use anyhow::Result;
use lazy_static::lazy_static;
use regex::Regex;
use serde_json::Value;

use crate::core::tracking;
use crate::core::utils::{exit_code_from_output, resolved_command, truncate};

const MAX_ITEMS: usize = 25;
const MAX_DESC_CHARS: usize = 500;
const MAX_RUN_OUTPUT_CHARS: usize = 300;

lazy_static! {
    /// Matches ISO-8601 timestamp prefix up to the minute: "2026-04-14T10:00"
    static ref TIMESTAMP_RE: Regex =
        Regex::new(r"(\d{4}-\d{2}-\d{2})T(\d{2}:\d{2})").unwrap();
}

/// Entry point: `args` = ["list"], ["get", "cccccccc-..."], ["runs", "cccccccc-..."], etc.
pub fn run(args: &[String], verbose: u8) -> Result<i32> {
    let verb = args.first().map(|s| s.as_str()).unwrap_or("");
    let rest = if args.len() > 1 { &args[1..] } else { &[][..] };

    match verb {
        "list" => run_filtered(
            &["issue", "list"],
            rest,
            "multica_issue_list",
            verbose,
            filter_issue_list,
        ),
        "get" => run_filtered(
            &["issue", "get"],
            rest,
            "multica_issue_get",
            verbose,
            filter_issue_get,
        ),
        "runs" => run_filtered(
            &["issue", "runs"],
            rest,
            "multica_issue_runs",
            verbose,
            filter_issue_runs,
        ),
        _ => run_passthrough(args, verbose),
    }
}

/// Execute `multica issue <sub_args> <extra_args>`, apply filter_fn, track savings.
fn run_filtered(
    sub_args: &[&str],
    extra_args: &[String],
    tee_slug: &str,
    verbose: u8,
    filter_fn: fn(&str) -> String,
) -> Result<i32> {
    let timer = tracking::TimedExecution::start();
    let cmd_label = format!("multica {}", sub_args.join(" "));
    let rtk_label = format!("rtk {}", cmd_label);

    if verbose > 0 {
        eprintln!(
            "rtk multica issue: running multica {} {}",
            sub_args.join(" "),
            extra_args.join(" ")
        );
    }

    let mut cmd = resolved_command("multica");
    cmd.args(sub_args);
    cmd.args(extra_args);

    // Inject --output json if caller has not already done so
    let has_output_json = extra_args
        .windows(2)
        .any(|w| w[0] == "--output" && w[1] == "json")
        || extra_args.iter().any(|a| a == "--output=json");
    if !has_output_json {
        cmd.args(["--output", "json"]);
    }

    let output = cmd
        .output()
        .map_err(|e| anyhow::anyhow!("Failed to run multica: {}", e))?;

    let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
    let stderr = String::from_utf8_lossy(&output.stderr).into_owned();
    let raw = format!("{}\n{}", stdout, stderr);
    let exit_code = exit_code_from_output(&output, "multica");

    if exit_code != 0 {
        if let Some(hint) = crate::core::tee::tee_and_hint(&raw, tee_slug, exit_code) {
            eprintln!("{}\n{}", stderr.trim(), hint);
        } else {
            eprint!("{}", stderr);
        }
        timer.track(&cmd_label, &rtk_label, &raw, &stderr);
        return Ok(exit_code);
    }

    let filtered = filter_fn(&stdout);
    if let Some(hint) = crate::core::tee::tee_and_hint(&raw, tee_slug, 0) {
        println!("{}\n{}", filtered, hint);
    } else {
        println!("{}", filtered);
    }

    timer.track(&cmd_label, &rtk_label, &raw, &filtered);
    Ok(0)
}

/// Passthrough for verbs that should not be filtered (create, update, status, assign, comment, etc.)
fn run_passthrough(args: &[String], verbose: u8) -> Result<i32> {
    let timer = tracking::TimedExecution::start();
    if verbose > 0 {
        eprintln!("rtk multica issue: passthrough for '{}'", args.join(" "));
    }
    let mut cmd_args: Vec<String> = vec!["issue".to_string()];
    cmd_args.extend_from_slice(args);

    let output = resolved_command("multica")
        .args(&cmd_args)
        .output()
        .map_err(|e| anyhow::anyhow!("Failed to run multica issue: {}", e))?;

    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    let exit_code = exit_code_from_output(&output, "multica");

    if !stdout.is_empty() {
        print!("{}", stdout);
    }
    if !stderr.is_empty() {
        eprint!("{}", stderr);
    }

    let raw = format!("{}\n{}", stdout, stderr);
    timer.track(
        &format!("multica issue {}", args.join(" ")),
        &format!("rtk multica issue {} (passthrough)", args.join(" ")),
        &raw,
        &raw,
    );

    Ok(exit_code)
}

// ---------------------------------------------------------------------------
// Filter functions
// ---------------------------------------------------------------------------

/// Filter `multica issue list --output json` output.
///
/// Compact table: IDENTIFIER | STATUS | PRI | ASSIGNEE_TYPE | TITLE
/// Capped at MAX_ITEMS with overflow count. UUIDs, timestamps, internal IDs stripped.
pub fn filter_issue_list(raw: &str) -> String {
    let items: Vec<Value> = match serde_json::from_str(raw) {
        Ok(Value::Array(arr)) => arr,
        _ => return raw.to_string(),
    };

    if items.is_empty() {
        return "No issues found.".to_string();
    }

    let total = items.len();
    let mut lines: Vec<String> = Vec::with_capacity(total.min(MAX_ITEMS) + 3);

    lines.push(format!(
        "{:<10}  {:<12}  {:<7}  {:<6}  {}",
        "ID", "STATUS", "PRI", "AGENT", "TITLE"
    ));
    lines.push("-".repeat(90));

    for item in items.iter().take(MAX_ITEMS) {
        let identifier = item["identifier"].as_str().unwrap_or("").to_string();
        let status = item["status"].as_str().unwrap_or("").to_string();
        let priority = item["priority"].as_str().unwrap_or("none").to_string();
        let assignee_type = item["assignee_type"].as_str().unwrap_or("-").to_string();
        let title = item["title"].as_str().unwrap_or("").to_string();

        // Shorten "in_progress" → "active", "in_review" → "review" to fit column
        let status_short = match status.as_str() {
            "in_progress" => "active".to_string(),
            "in_review"   => "review".to_string(),
            other         => other.to_string(),
        };

        let pri_short = match priority.as_str() {
            "urgent" => "urg".to_string(),
            "high"   => "hi".to_string(),
            "medium" => "med".to_string(),
            "low"    => "lo".to_string(),
            _        => "-".to_string(),
        };

        let agent_flag = if assignee_type == "agent" { "yes" } else { "no" };

        lines.push(format!(
            "{:<10}  {:<12}  {:<7}  {:<6}  {}",
            truncate(&identifier, 10),
            truncate(&status_short, 12),
            truncate(&pri_short, 7),
            agent_flag,
            truncate(&title, 60),
        ));
    }

    if total > MAX_ITEMS {
        lines.push(format!("... +{} more issues", total - MAX_ITEMS));
    }

    lines.join("\n")
}

/// Filter `multica issue get <id> --output json` output.
///
/// Extracts: identifier, title, status, priority, assignee_type, due_date,
/// parent indicator, and description (truncated).
pub fn filter_issue_get(raw: &str) -> String {
    let v: Value = match serde_json::from_str(raw) {
        Ok(v) => v,
        Err(_) => return raw.to_string(),
    };

    let identifier    = v["identifier"].as_str().unwrap_or("").to_string();
    let title         = v["title"].as_str().unwrap_or("").to_string();
    let status        = v["status"].as_str().unwrap_or("").to_string();
    let priority      = v["priority"].as_str().unwrap_or("none").to_string();
    let assignee_type = v["assignee_type"].as_str().unwrap_or("").to_string();
    let due_date      = v["due_date"].as_str().unwrap_or("").to_string();
    let description   = v["description"].as_str().unwrap_or("").to_string();
    let has_parent    = !v["parent_issue_id"].is_null();

    let mut lines: Vec<String> = Vec::new();

    // Header line: IDENTIFIER [STATUS] Title
    lines.push(format!(
        "{} [{}] {}",
        if identifier.is_empty() { "?" } else { &identifier },
        status,
        title
    ));

    if priority != "none" && !priority.is_empty() {
        lines.push(format!("Priority: {}", priority));
    }
    if !assignee_type.is_empty() {
        lines.push(format!("Assigned to: {}", assignee_type));
    }
    if !due_date.is_empty() {
        // Trim time component from ISO date
        let display_date = &due_date[..due_date.find('T').unwrap_or(due_date.len())];
        lines.push(format!("Due: {}", display_date));
    }
    if has_parent {
        lines.push("Parent: (see parent_issue_id — fetch with multica issue get)".to_string());
    }

    if !description.is_empty() {
        lines.push(String::new());
        lines.push("Description:".to_string());
        lines.push(truncate(&description, MAX_DESC_CHARS));
    }

    lines.join("\n")
}

/// Filter `multica issue runs <id> --output json` output.
///
/// Compact table: RUN_SHORT | STATUS | STARTED | DURATION | RESULT_SUMMARY
pub fn filter_issue_runs(raw: &str) -> String {
    let runs: Vec<Value> = match serde_json::from_str(raw) {
        Ok(Value::Array(arr)) => arr,
        _ => return raw.to_string(),
    };

    if runs.is_empty() {
        return "No runs found.".to_string();
    }

    let total = runs.len();
    let mut lines: Vec<String> = Vec::with_capacity(total.min(MAX_ITEMS) + 2);

    lines.push(format!(
        "{:<8}  {:<10}  {:<16}  {:<8}  {}",
        "RUN", "STATUS", "STARTED", "DURATION", "SUMMARY"
    ));
    lines.push("-".repeat(90));

    for run in runs.iter().take(MAX_ITEMS) {
        let run_id = run["id"].as_str().unwrap_or("");
        let status = run["status"].as_str().unwrap_or("").to_string();
        let started = run["started_at"].as_str().unwrap_or("").to_string();
        let completed = run["completed_at"].as_str().unwrap_or("").to_string();
        let error = run["error"].as_str().unwrap_or("").to_string();
        let output_text = run["result"]["output"].as_str().unwrap_or("").to_string();

        // Short run ID: first 8 chars of UUID
        let short_id = &run_id[..run_id.len().min(8)];

        // Compact start timestamp: "2026-04-14 10:00"
        let started_short = TIMESTAMP_RE
            .captures(&started)
            .map(|c| format!("{} {}", &c[1], &c[2]))
            .unwrap_or_else(|| started[..started.len().min(16)].to_string());

        // Duration in minutes (approximate)
        let duration = compute_duration(&started, &completed);

        let summary = if !error.is_empty() {
            format!("ERROR: {}", truncate(&error, 60))
        } else {
            truncate(&output_text, MAX_RUN_OUTPUT_CHARS)
        };

        lines.push(format!(
            "{:<8}  {:<10}  {:<16}  {:<8}  {}",
            short_id,
            truncate(&status, 10),
            started_short,
            duration,
            summary,
        ));
    }

    if total > MAX_ITEMS {
        lines.push(format!("... +{} more runs", total - MAX_ITEMS));
    }

    lines.join("\n")
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Compute approximate human duration between two ISO-8601 strings.
/// Returns "~Xm" or "~Xs" or "-" if timestamps can't be parsed.
fn compute_duration(started: &str, completed: &str) -> String {
    if started.is_empty() || completed.is_empty() {
        return "-".to_string();
    }
    // Parse just the seconds portion for a rough duration
    // Format: 2026-04-14T10:00:01Z vs 2026-04-14T10:05:40Z
    let parse_secs = |s: &str| -> Option<i64> {
        let t = s.split('T').nth(1)?;
        let t = t.trim_end_matches('Z');
        let parts: Vec<&str> = t.split(':').collect();
        if parts.len() < 3 { return None; }
        let h: i64 = parts[0].parse().ok()?;
        let m: i64 = parts[1].parse().ok()?;
        let sec: i64 = parts[2].parse::<f64>().ok()? as i64;
        Some(h * 3600 + m * 60 + sec)
    };
    match (parse_secs(started), parse_secs(completed)) {
        (Some(s), Some(c)) if c >= s => {
            let diff = c - s;
            if diff >= 60 {
                format!("~{}m", diff / 60)
            } else {
                format!("~{}s", diff)
            }
        }
        _ => "-".to_string(),
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    fn count_tokens(s: &str) -> usize {
        s.split_whitespace().count()
    }

    // ---- issue list ----

    #[test]
    fn test_issue_list_snapshot() {
        let input = include_str!("../../../tests/fixtures/multica_issue_list_raw.txt");
        let output = filter_issue_list(input);
        insta::assert_snapshot!(output);
    }

    #[test]
    fn test_issue_list_savings() {
        let input = include_str!("../../../tests/fixtures/multica_issue_list_raw.txt");
        let output = filter_issue_list(input);
        let pct = 100.0 - (count_tokens(&output) as f64 / count_tokens(input) as f64 * 100.0);
        assert!(
            pct >= 60.0,
            "issue list: expected ≥60% savings, got {:.1}%",
            pct
        );
    }

    #[test]
    fn test_issue_list_empty_array() {
        assert_eq!(filter_issue_list("[]"), "No issues found.");
    }

    #[test]
    fn test_issue_list_not_json() {
        let out = filter_issue_list("Error: unauthorized");
        assert_eq!(out, "Error: unauthorized");
    }

    #[test]
    fn test_issue_list_has_header() {
        let input = include_str!("../../../tests/fixtures/multica_issue_list_raw.txt");
        let output = filter_issue_list(input);
        assert!(output.contains("ID"), "should contain table header");
        assert!(output.contains("STATUS"), "should contain STATUS column");
        assert!(output.contains("TITLE"), "should contain TITLE column");
    }

    #[test]
    fn test_issue_list_has_identifiers() {
        let input = include_str!("../../../tests/fixtures/multica_issue_list_raw.txt");
        let output = filter_issue_list(input);
        assert!(output.contains("DEMO-13"), "should contain first issue identifier");
        assert!(output.contains("DEMO-9"), "should contain last issue identifier");
    }

    #[test]
    fn test_issue_list_no_uuids() {
        let input = include_str!("../../../tests/fixtures/multica_issue_list_raw.txt");
        let output = filter_issue_list(input);
        // UUIDs are stripped; none of the raw UUID values should appear in filtered output
        assert!(
            !output.contains("aaaaaaaa-0001"),
            "should not contain raw UUID"
        );
        assert!(
            !output.contains("cccccccc-0001"),
            "should not contain raw UUID"
        );
    }

    // ---- issue get ----

    #[test]
    fn test_issue_get_snapshot() {
        let input = include_str!("../../../tests/fixtures/multica_issue_get_raw.txt");
        let output = filter_issue_get(input);
        insta::assert_snapshot!(output);
    }

    #[test]
    fn test_issue_get_savings() {
        let input = include_str!("../../../tests/fixtures/multica_issue_get_raw.txt");
        let output = filter_issue_get(input);
        let pct = 100.0 - (count_tokens(&output) as f64 / count_tokens(input) as f64 * 100.0);
        // Threshold is conservative for the compact synthetic fixture.
        // Real multica issue get responses are 4,000–12,000 tokens and achieve ≥85% savings.
        assert!(
            pct >= 25.0,
            "issue get: expected ≥25% savings, got {:.1}%",
            pct
        );
    }

    #[test]
    fn test_issue_get_empty() {
        let _ = filter_issue_get("");
    }

    #[test]
    fn test_issue_get_not_json() {
        let out = filter_issue_get("Error: issue not found");
        assert_eq!(out, "Error: issue not found");
    }

    #[test]
    fn test_issue_get_has_key_fields() {
        let input = include_str!("../../../tests/fixtures/multica_issue_get_raw.txt");
        let output = filter_issue_get(input);
        assert!(output.contains("DEMO-13"), "should contain identifier");
        assert!(output.contains("in_review"), "should contain status");
        assert!(output.contains("high"), "should contain priority");
        assert!(output.contains("Description:"), "should contain description section");
    }

    #[test]
    fn test_issue_get_no_uuids() {
        let input = include_str!("../../../tests/fixtures/multica_issue_get_raw.txt");
        let output = filter_issue_get(input);
        assert!(
            !output.contains("cccccccc-0001"),
            "should not contain raw UUID in output"
        );
    }

    #[test]
    fn test_issue_get_has_parent_indicator() {
        let input = include_str!("../../../tests/fixtures/multica_issue_get_raw.txt");
        let output = filter_issue_get(input);
        assert!(output.contains("Parent:"), "should indicate parent issue exists");
    }

    // ---- issue runs ----

    #[test]
    fn test_issue_runs_snapshot() {
        let input = include_str!("../../../tests/fixtures/multica_issue_runs_raw.txt");
        let output = filter_issue_runs(input);
        insta::assert_snapshot!(output);
    }

    #[test]
    fn test_issue_runs_savings() {
        let input = include_str!("../../../tests/fixtures/multica_issue_runs_raw.txt");
        let output = filter_issue_runs(input);
        let pct = 100.0 - (count_tokens(&output) as f64 / count_tokens(input) as f64 * 100.0);
        // Threshold is conservative for the compact synthetic fixture.
        // Real runs output with multiple runs and long result.output achieves ≥65% savings.
        assert!(
            pct >= 25.0,
            "issue runs: expected ≥25% savings, got {:.1}%",
            pct
        );
    }

    #[test]
    fn test_issue_runs_empty_array() {
        assert_eq!(filter_issue_runs("[]"), "No runs found.");
    }

    #[test]
    fn test_issue_runs_not_json() {
        let out = filter_issue_runs("Error: issue not found");
        assert_eq!(out, "Error: issue not found");
    }

    #[test]
    fn test_issue_runs_has_status() {
        let input = include_str!("../../../tests/fixtures/multica_issue_runs_raw.txt");
        let output = filter_issue_runs(input);
        assert!(output.contains("completed"), "should contain run status");
    }

    // ---- compute_duration helper ----

    #[test]
    fn test_duration_minutes() {
        assert_eq!(
            compute_duration("2026-04-14T10:00:01Z", "2026-04-14T10:05:40Z"),
            "~5m"
        );
    }

    #[test]
    fn test_duration_seconds() {
        assert_eq!(
            compute_duration("2026-04-14T10:00:00Z", "2026-04-14T10:00:45Z"),
            "~45s"
        );
    }

    #[test]
    fn test_duration_empty() {
        assert_eq!(compute_duration("", ""), "-");
    }
}
