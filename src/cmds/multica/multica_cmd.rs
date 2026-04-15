//! Multica managed-agents CLI proxy with token-optimized output.
//!
//! Routes `rtk multica <subcommand> [args...]` to subcommand-specific filter modules.
//! Unknown subcommands passthrough to the real multica binary unchanged.

use anyhow::Result;
use crate::core::utils::{exit_code_from_output, resolved_command};
use crate::core::tracking;

/// Entry point called from main.rs routing arm.
pub fn run(subcommand: &str, args: &[String], verbose: u8) -> Result<i32> {
    match subcommand {
        "issue"     => super::issue::run(args, verbose),
        "workspace" => super::workspace::run(args, verbose),
        _           => run_passthrough(subcommand, args, verbose),
    }
}

/// Execute multica without filtering for unrecognised subcommands
/// (daemon, config, auth, setup, update, etc.)
fn run_passthrough(subcommand: &str, args: &[String], verbose: u8) -> Result<i32> {
    let timer = tracking::TimedExecution::start();
    if verbose > 0 {
        eprintln!("rtk multica: passthrough for subcommand '{}'", subcommand);
    }
    let mut cmd_args: Vec<String> = vec![subcommand.to_string()];
    cmd_args.extend_from_slice(args);

    let output = resolved_command("multica")
        .args(&cmd_args)
        .output()
        .map_err(|e| anyhow::anyhow!("Failed to run multica: {}", e))?;

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
        &format!("multica {} {}", subcommand, args.join(" ")),
        &format!("rtk multica {} {} (passthrough)", subcommand, args.join(" ")),
        &raw,
        &raw,
    );

    Ok(exit_code)
}
