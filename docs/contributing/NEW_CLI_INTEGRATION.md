# Adding a New CLI Tool to RTK

This guide walks through the complete process of integrating any new CLI tool as a first-class RTK filter. It is the canonical reference for all future CLI integrations — follow it in order, commit after each step.

> **Reference integrations:** `acli` (ADR-001) and `multica` (ADR-002) are the canonical examples. Read their source (`src/cmds/atlassian/`, `src/cmds/multica/`) before starting a new one.

---

## When to integrate a CLI tool

Integrate a tool when **all three** are true:

1. The tool is invoked by AI agents (not just humans interactively)
2. Its output contains ≥60% noise relative to what the agent uses
3. It is invoked with enough frequency to justify the maintenance surface

If a tool is invoked once per session and its output is already compact, a TOML filter is sufficient. Use the full Rust module path only when JSON output needs structural extraction.

---

## Decision: Tier 1 (TOML) vs Tier 2 (Rust)

| Criterion | Tier 1 — TOML | Tier 2 — Rust |
|---|---|---|
| Output format | Plain text, tables | JSON (nested or flat) |
| Fields to extract | Lines to strip | Specific JSON keys to keep |
| Time to ship | 15 min | 1–2 hrs |
| When to upgrade | When savings drop below 60% | When TOML cannot parse the format |

**Default rule:** Start with TOML. Promote to Rust only when `strip_lines_matching` cannot achieve the savings target.

---

## Step 0 — Pre-flight checks

```bash
# Verify working directory
pwd  # must be the rtk project root
git status  # should be clean

# Create feature branch
git checkout -b feature/<toolname>-filter

# Verify the tool is installed locally
which <toolname>
<toolname> --version
<toolname> --help
```

---

## Step 1 — Capture fixtures

Fixtures are real command output captured from the actual binary. **Never use synthetic data.**

```bash
# For each command you plan to filter:
<toolname> <subcommand> [--output json or --json] > tests/fixtures/<toolname>_<subcommand>_raw.txt

# Examples:
multica issue list --output json > tests/fixtures/multica_issue_list_raw.txt
acli jira workitem view AWM-1 --json > tests/fixtures/acli_jira_workitem_view_raw.txt
```

**Fixture hygiene (critical for open-source repos):**
- Replace real issue keys with `DEMO-1234` style
- Replace real user names with `Alice Developer` / `Bob Reviewer`
- Replace internal URLs with `https://example.atlassian.net` style
- Remove emails, API tokens, internal hostnames
- Keep the JSON structure and field names exactly as the real output

Document how to regenerate each fixture at the top of the fixture file as a comment:
```
# Generated: multica issue list --output json
# Sanitized: replaced real workspace/user data with synthetic equivalents
```

---

## Step 2 — Decide command coverage

For each command the tool exposes, assign one of three dispositions:

| Disposition | Description | Criteria |
|---|---|---|
| **Filter (Rust)** | Parse and restructure output | JSON, nested structure, field extraction |
| **Filter (TOML)** | Strip lines matching patterns | Plain text, tables, line-level noise |
| **Passthrough** | Execute unchanged | Write operations, interactive flows, auth |

Write this table down before writing any code. It becomes §4 of the ADR.

**Passthrough is always correct for:**
- `create`, `edit`, `update`, `delete` — confirmation output is the signal
- `auth`, `login`, `logout` — interactive terminal flows
- `install`, `setup`, `upgrade` — lifecycle management
- Any command where the agent needs the full output to decide next action

---

## Step 3 — TOML filters (Tier 1)

For each Tier 1 command, create `src/filters/<toolname>-<subcommand>.toml`:

```toml
[filter]
command = "<toolname> <subcommand>"

# Lines matching any of these patterns are removed
strip_lines_matching = [
  "^\\s*$",          # blank lines
  "uuid|UUID",        # internal IDs
  "createdAt|updatedAt",
  "^Fetching",        # progress noise
]

# Hard cap on output length
max_lines = 30
```

These are auto-embedded by `build.rs` on the next `cargo build`. No Rust changes required.

Verify the filter works:
```bash
cargo build
cat tests/fixtures/<toolname>_<subcommand>_raw.txt | rtk proxy <toolname> <subcommand>
# compare raw vs filtered line count
```

---

## Step 4 — Rust module skeleton (Tier 2)

### 4a. Create the directory

```
src/cmds/<toolname>/
├── mod.rs
├── <toolname>_cmd.rs    ← top-level router
└── <subcommand>.rs      ← per-surface dispatch (one per logical surface)
```

**mod.rs** — minimal, uses automod:
```rust
automod::dir!(pub "src/cmds/<toolname>");
```

**<toolname>_cmd.rs** — routes by first arg:
```rust
pub fn run(subcommand: &str, args: &[String], verbose: u8) -> anyhow::Result<i32> {
    match subcommand {
        "<surface1>" => super::<surface1>::run(args, verbose),
        "<surface2>" => super::<surface2>::run(args, verbose),
        _            => run_passthrough(subcommand, args, verbose),
    }
}
```

**<subcommand>.rs** — dispatch table, all passthrough stubs initially:
```rust
pub fn run(args: &[String], verbose: u8) -> anyhow::Result<i32> {
    let verb = args.first().map(|s| s.as_str()).unwrap_or("");
    let rest = if args.len() > 1 { &args[1..] } else { &[][..] };
    match verb {
        "list"  => run_filtered(&["<surface>","list"], rest, "<slug>", verbose, filter_list),
        "get"   => run_filtered(&["<surface>","get"],  rest, "<slug>", verbose, filter_get),
        _       => run_passthrough(args, verbose),
    }
}

fn filter_list(raw: &str) -> String { raw.to_string() }  // stub
fn filter_get(raw: &str)  -> String { raw.to_string() }  // stub
```

### 4b. Implement `run_filtered` and `run_passthrough`

Copy the pattern verbatim from `src/cmds/atlassian/jira.rs::run_filtered()`. The function:
1. Starts a `tracking::TimedExecution`
2. Builds the command with `resolved_command("<toolname>")`
3. Injects `--output json` / `--json` if not already present (check the tool's flag)
4. Runs the command
5. On non-zero exit: tee + passthrough stderr
6. On success: apply `filter_fn`, tee, print
7. Calls `timer.track(cmd_label, rtk_label, raw, filtered)`

Key variable to adapt: the JSON flag injection pattern:
```rust
// acli uses --json:
let needs_json = !extra_args.iter().any(|a| a == "--json");
if needs_json { cmd.arg("--json"); }

// multica uses --output json:
let needs_json = !extra_args.windows(2).any(|w| w[0] == "--output" && w[1] == "json");
if needs_json { cmd.args(&["--output", "json"]); }
```

### 4c. Compile check

```bash
cargo build 2>&1 | head -30
# Fix any compile errors before proceeding
```

---

## Step 5 — Wire main.rs

Three lines to add:

```rust
// 1. At the top, with other use statements:
use cmds::<toolname>::<toolname>_cmd;

// 2. In the Commands enum (add in alphabetical order with other tools):
/// <Toolname> CLI with token-optimized output
<Toolname> {
    /// Subcommand: <surface1>, <surface2>, etc.
    subcommand: String,
    #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
    args: Vec<String>,
},

// 3. In run_cli() match arm (maintain alphabetical order):
Commands::<Toolname> { subcommand, args } => <toolname>_cmd::run(&subcommand, &args, cli.verbose)?,
```

```bash
cargo build  # must pass clean
```

---

## Step 6 — Add discovery rule

In `src/discover/rules.rs`, add one `RtkRule` entry:

```rust
RtkRule {
    pattern: r"^<toolname>\s+(<surface1>|<surface2>)\b",
    rtk_cmd: "rtk <toolname>",
    rewrite_prefixes: &["<toolname>"],
    category: "<Category>",
    savings_pct: 75.0,  // weighted average across filtered commands
    subcmd_savings: &[
        ("<surface1> list", 75.0),
        ("<surface1> get",  85.0),
    ],
    subcmd_status: &[
        ("<surface1> create", RtkStatus::Passthrough),
        ("auth",              RtkStatus::Passthrough),
    ],
},
```

Verify rewrite works:
```bash
cargo build
rtk rewrite "<toolname> <surface1> list"
# Expected: rtk <toolname> <surface1> list

rtk rewrite "<toolname> auth login"
# Expected: <toolname> auth login  (not rewritten)
```

---

## Step 7 — Implement filter functions

For each filter stub (replaced the `raw.to_string()` return):

### JSON array (e.g., a list command)

```rust
fn filter_list(raw: &str) -> String {
    let items: Vec<Value> = match serde_json::from_str(raw) {
        Ok(Value::Array(arr)) => arr,
        _ => return raw.to_string(),  // fallback on parse failure
    };

    if items.is_empty() {
        return "No items found.".to_string();
    }

    let total = items.len();
    let mut lines = Vec::with_capacity(total.min(MAX_ITEMS) + 2);

    // Header
    lines.push(format!("{:<12} {:<12} {:<16} {}", "ID", "STATUS", "ASSIGNEE", "TITLE"));
    lines.push("-".repeat(80));

    for item in items.iter().take(MAX_ITEMS) {
        let id      = item["identifier"].as_str().unwrap_or("").to_string();
        let status  = item["status"]["name"].as_str().unwrap_or("").to_string();
        let assignee = item["assignee"]["name"].as_str().unwrap_or("").to_string();
        let title   = item["title"].as_str().unwrap_or("").to_string();
        lines.push(format!("{:<12} {:<12} {:<16} {}",
            truncate(&id, 12), truncate(&status, 12),
            truncate(&assignee, 16), truncate(&title, 60)));
    }

    if total > MAX_ITEMS {
        lines.push(format!("... +{} more", total - MAX_ITEMS));
    }

    lines.join("\n")
}
```

### JSON object (e.g., a get/view command)

```rust
fn filter_get(raw: &str) -> String {
    let v: Value = match serde_json::from_str(raw) {
        Ok(v) => v,
        Err(_) => return raw.to_string(),  // fallback
    };

    let id          = v["identifier"].as_str().unwrap_or("").to_string();
    let title       = v["title"].as_str().unwrap_or("").to_string();
    let status      = v["status"]["name"].as_str().unwrap_or("").to_string();
    let priority    = v["priorityLabel"].as_str().unwrap_or("").to_string();
    let assignee    = v["assignee"]["name"].as_str().unwrap_or("").to_string();
    let description = v["description"].as_str().unwrap_or("").to_string();

    let mut lines = Vec::new();

    lines.push(format!("{} {}", id, title));
    if !status.is_empty()   { lines.push(format!("Status: {}", status)); }
    if !priority.is_empty() { lines.push(format!("Priority: {}", priority)); }
    if !assignee.is_empty() { lines.push(format!("Assignee: {}", assignee)); }

    if !description.is_empty() {
        lines.push(String::new());
        lines.push("Description:".to_string());
        lines.push(truncate(&description, MAX_DESC_CHARS));
    }

    lines.join("\n")
}
```

**Critical rules for filter functions:**
- Always return `raw.to_string()` on parse failure — never panic, never return empty on failure
- Truncate long strings with `truncate()` from `core::utils`
- Cap list output at `MAX_ITEMS` (25 is standard)
- Never include UUIDs, timestamps, internal IDs, or pagination metadata

---

## Step 8 — Tests

For every filter function, add three test categories in `#[cfg(test)] mod tests`:

### 1. Snapshot test
```rust
#[test]
fn test_list_snapshot() {
    let input = include_str!("../../../tests/fixtures/<toolname>_list_raw.txt");
    insta::assert_snapshot!(filter_list(input));
}
```

### 2. Token savings test
```rust
fn count_tokens(s: &str) -> usize { s.split_whitespace().count() }

#[test]
fn test_list_savings() {
    let input  = include_str!("../../../tests/fixtures/<toolname>_list_raw.txt");
    let output = filter_list(input);
    let pct    = 100.0 - (count_tokens(&output) as f64 / count_tokens(input) as f64 * 100.0);
    assert!(pct >= 60.0, "expected ≥60% savings, got {:.1}%", pct);
}
```

### 3. Robustness tests
```rust
#[test] fn test_list_empty_array() { assert_eq!(filter_list("[]"), "No items found."); }
#[test] fn test_list_not_json()    { let _ = filter_list("Error: unauthorized"); }
#[test] fn test_list_empty_str()   { let _ = filter_list(""); }
```

Run all tests and accept snapshots:
```bash
cargo test --all
cargo insta review   # press 'a' to accept each snapshot
```

---

## Step 9 — Quality gate

```bash
# Must all pass before committing
cargo fmt --all
cargo clippy --all-targets   # zero warnings
cargo test --all

# Benchmark startup overhead
hyperfine "rtk <toolname> <surface> list" --warmup 3
# RTK overhead should be <10ms vs raw command
```

---

## Step 10 — ADR and documentation

1. **Write an ADR** in the ObsiZone vault at:
   `ObsiZone/Engineering/ADR-00N - RTK x <toolname> Integration.md`
   
   Follow the structure of ADR-001 (acli) or ADR-002 (multica). Include:
   - Context: what the tool is and how agents use it
   - Problem: token metrics (raw vs useful)
   - Decision: two-tier approach, what's filtered vs passthrough
   - Command surface table with savings targets
   - Module architecture with file structure
   - Implementation plan (Steps 0–7 from this guide)
   - Verification criteria (the checklist below)

2. **Update `src/cmds/README.md`** — add one line to the command filter table.

3. **Commit** following the project convention:
   ```
   feat: add <toolname> as first-class filter family in RTK
   ```

---

## Verification checklist (ship gate)

Before marking the integration complete:

- [ ] `rtk rewrite "<toolname> <surface> list"` → `rtk <toolname> <surface> list`
- [ ] `rtk rewrite "<toolname> auth login"` → `<toolname> auth login` (not rewritten)
- [ ] `rtk <toolname> <surface> list` returns ≥75% fewer tokens than raw
- [ ] `rtk <toolname> <surface> get <id>` returns ≥85% fewer tokens than raw
- [ ] Filtered output contains all fields the agent needs for decision-making
- [ ] `cargo test --all` passes
- [ ] `cargo insta review` — all snapshots accepted
- [ ] Startup overhead <10ms (`hyperfine`)
- [ ] Passthrough confirmed: write operations behave identically to raw binary
- [ ] Unknown subcommand passthrough: `rtk <toolname> <unknown>` invokes `<toolname> <unknown>` unchanged
- [ ] `rtk gain` shows the new category in savings breakdown
- [ ] ADR written and filed in ObsiZone

---

## Common mistakes

| Mistake | Fix |
|---|---|
| Returning `""` on parse failure | Return `raw.to_string()` — always fallback |
| Injecting `--json` when tool uses `--output json` | Check the tool's flag; inject the correct one |
| Forgetting to cap list output | Add `items.iter().take(MAX_ITEMS)` + overflow line |
| Including UUIDs in output | They consume tokens and add no agent value; strip them |
| Snapshot test with `raw.to_string()` stub | Implement the real filter *before* accepting the snapshot |
| `cargo build` before wiring main.rs | Module must exist before adding the `use` import |
| Missing `run_passthrough` for unhandled verbs | Every dispatch table needs a `_ =>` arm |
| Testing with synthetic data | Always use `include_str!("../../../tests/fixtures/...")` |
| Skipping the `cargo insta review` step | Snapshots must be reviewed and accepted, not auto-committed |

---

## Reference implementations

| Tool | Module | ADR | Tier |
|---|---|---|---|
| `acli jira` | `src/cmds/atlassian/jira.rs` | ADR-001 | 2 (Rust, ADF JSON) |
| `acli confluence` | `src/cmds/atlassian/confluence.rs` | ADR-001 | 2 (Rust, ADF JSON) |
| `multica issue` | `src/cmds/multica/issue.rs` | ADR-002 | 2 (Rust, flat JSON) |
| `multica workspace` | `src/cmds/multica/workspace.rs` | ADR-002 | 2 (Rust, flat JSON) |
| `gh pr/issue/run` | `src/cmds/git/gh_cmd.rs` | — | 2 (Rust) |
| `docker ps/images` | `src/cmds/cloud/container.rs` | — | 2 (Rust) |
