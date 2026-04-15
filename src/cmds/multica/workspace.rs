//! Multica workspace filter module — `rtk multica workspace <verb> [args...]`
//!
//! Workspace list uses table format (no JSON flag). Strips UUID columns.
//! Unknown verbs (get, members, watch, unwatch) fallthrough to passthrough.

use anyhow::Result;
use crate::core::tracking;
use crate::core::utils::{exit_code_from_output, resolved_command};

/// Entry point: `args` = ["list"], ["get", "<id>"], ["members", "<id>"], etc.
pub fn run(args: &[String], verbose: u8) -> Result<i32> {
    let verb = args.first().map(|s| s.as_str()).unwrap_or("");
    let rest = if args.len() > 1 { &args[1..] } else { &[][..] };

    match verb {
        "list" => run_workspace_list(rest, verbose),
        _      => run_passthrough(args, verbose),
    }
}

/// Run `multica workspace list` and strip the UUID ID column, keeping NAME and WATCHING.
fn run_workspace_list(extra_args: &[String], verbose: u8) -> Result<i32> {
    let timer = tracking::TimedExecution::start();
    let cmd_label = "multica workspace list".to_string();
    let rtk_label = format!("rtk {}", cmd_label);
    let tee_slug = "multica_workspace_list";

    if verbose > 0 {
        eprintln!("rtk multica workspace: running multica workspace list");
    }

    let output = resolved_command("multica")
        .args(["workspace", "list"])
        .args(extra_args)
        .output()
        .map_err(|e| anyhow::anyhow!("Failed to run multica workspace list: {}", e))?;

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

    let filtered = filter_workspace_list(&stdout);
    if let Some(hint) = crate::core::tee::tee_and_hint(&raw, tee_slug, 0) {
        println!("{}\n{}", filtered, hint);
    } else {
        println!("{}", filtered);
    }

    timer.track(&cmd_label, &rtk_label, &raw, &filtered);
    Ok(0)
}

/// Passthrough for unhandled workspace verbs (get, members, watch, unwatch).
fn run_passthrough(args: &[String], verbose: u8) -> Result<i32> {
    let timer = tracking::TimedExecution::start();
    if verbose > 0 {
        eprintln!("rtk multica workspace: passthrough for '{}'", args.join(" "));
    }
    let mut cmd_args: Vec<String> = vec!["workspace".to_string()];
    cmd_args.extend_from_slice(args);

    let output = resolved_command("multica")
        .args(&cmd_args)
        .output()
        .map_err(|e| anyhow::anyhow!("Failed to run multica workspace: {}", e))?;

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
        &format!("multica workspace {}", args.join(" ")),
        &format!("rtk multica workspace {} (passthrough)", args.join(" ")),
        &raw,
        &raw,
    );

    Ok(exit_code)
}

// ---------------------------------------------------------------------------
// Filter functions
// ---------------------------------------------------------------------------

/// Filter `multica workspace list` table output.
///
/// Raw output has columns: ID (UUID) | NAME | WATCHING
/// Filtered output: NAME | WATCHING (UUIDs stripped)
pub fn filter_workspace_list(raw: &str) -> String {
    let mut lines: Vec<String> = Vec::new();
    let mut header_seen = false;

    for line in raw.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }

        // Header line: "ID   NAME   WATCHING"
        if trimmed.starts_with("ID") && !header_seen {
            header_seen = true;
            lines.push(format!("{:<20}  {}", "NAME", "WATCHING"));
            lines.push("-".repeat(30));
            continue;
        }

        // Data lines start with a UUID (8-4-4-4-12 hex pattern)
        // Split on 2+ spaces to find columns
        let cols: Vec<&str> = line.splitn(3, "  ").map(str::trim).collect();
        if cols.len() >= 2 {
            // cols[0] = UUID, cols[1] = NAME, cols[2] = WATCHING (optional)
            let name     = cols.get(1).copied().unwrap_or("");
            let watching = cols.get(2).copied().unwrap_or("").trim();
            if !name.is_empty() {
                lines.push(format!("{:<20}  {}", name, watching));
            }
        }
    }

    if lines.is_empty() {
        return "No workspaces found.".to_string();
    }

    lines.join("\n")
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

    #[test]
    fn test_workspace_list_snapshot() {
        let input = include_str!("../../../tests/fixtures/multica_workspace_list_raw.txt");
        let output = filter_workspace_list(input);
        insta::assert_snapshot!(output);
    }

    #[test]
    fn test_workspace_list_savings() {
        let input = include_str!("../../../tests/fixtures/multica_workspace_list_raw.txt");
        let output = filter_workspace_list(input);
        let pct = 100.0 - (count_tokens(&output) as f64 / count_tokens(input) as f64 * 100.0);
        // Workspace list is already a compact table; savings come from stripping UUID column.
        // With a single-workspace fixture the percentage is modest by design.
        assert!(
            pct >= 5.0,
            "workspace list: expected ≥5% savings (UUID stripped), got {:.1}%",
            pct
        );
    }

    #[test]
    fn test_workspace_list_strips_uuid() {
        let input = include_str!("../../../tests/fixtures/multica_workspace_list_raw.txt");
        let output = filter_workspace_list(input);
        assert!(
            !output.contains("dddddddd-0001"),
            "UUID should be stripped from output"
        );
    }

    #[test]
    fn test_workspace_list_has_name() {
        let input = include_str!("../../../tests/fixtures/multica_workspace_list_raw.txt");
        let output = filter_workspace_list(input);
        assert!(output.contains("My Workspace"), "should contain workspace name");
    }

    #[test]
    fn test_workspace_list_empty() {
        let out = filter_workspace_list("");
        assert_eq!(out, "No workspaces found.");
    }

    #[test]
    fn test_workspace_list_watching_indicator() {
        let input = include_str!("../../../tests/fixtures/multica_workspace_list_raw.txt");
        let output = filter_workspace_list(input);
        assert!(output.contains('*'), "should preserve watching indicator");
    }
}
