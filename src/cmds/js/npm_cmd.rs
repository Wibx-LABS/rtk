//! Filters npm output and auto-injects the "run" subcommand when appropriate.

use crate::cmds::js::vitest_cmd;
use crate::core::runner;
use crate::core::utils::resolved_command;
use crate::Commands;
use anyhow::Result;
use std::fs;

/// Known npm subcommands that should NOT get "run" injected.
/// Shared between production code and tests to avoid drift.
const NPM_SUBCOMMANDS: &[&str] = &[
    "install",
    "i",
    "ci",
    "uninstall",
    "remove",
    "rm",
    "update",
    "up",
    "list",
    "ls",
    "outdated",
    "init",
    "create",
    "publish",
    "pack",
    "link",
    "audit",
    "fund",
    "exec",
    "explain",
    "why",
    "search",
    "view",
    "info",
    "show",
    "config",
    "set",
    "get",
    "cache",
    "prune",
    "dedupe",
    "doctor",
    "help",
    "version",
    "prefix",
    "root",
    "bin",
    "bugs",
    "docs",
    "home",
    "repo",
    "ping",
    "whoami",
    "token",
    "profile",
    "team",
    "access",
    "owner",
    "deprecate",
    "dist-tag",
    "star",
    "stars",
    "login",
    "logout",
    "adduser",
    "unpublish",
    "pkg",
    "diff",
    "rebuild",
    "test",
    "t",
    "start",
    "stop",
    "restart",
];

/// Test runners that have a dedicated RTK filter worth more than npm's generic one.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ScriptRunner {
    Jest,
    Vitest,
}

/// Shell syntax that makes a script more than a single runner invocation.
///
/// This list is a refusal, not a parser: the dedicated filters exec the runner
/// binary directly, so routing `"test": "vitest run && node --test tests/*.mjs"`
/// would silently drop the second half of the script. Anything shaped like more
/// than one command stays on the npm path, where the whole script still runs.
/// `&&` and `||` are absent on purpose: `&` and `|` already subsume them, and a
/// list with both lets a test pass for the wrong reason.
const SHELL_OPERATORS: &[&str] = &["|", "&", ";", ">", "<", "$(", "`", "\n"];

/// Read `scripts.<name>` from the package.json in the current directory.
fn script_body(name: &str) -> Option<String> {
    let raw = fs::read_to_string("package.json").ok()?;
    let json: serde_json::Value = serde_json::from_str(&raw).ok()?;
    json.get("scripts")?
        .get(name)?
        .as_str()
        .map(str::to_string)
}

/// Recognise a script that is exactly one test-runner invocation.
///
/// Returns the runner and the arguments the script itself passes. `None` means
/// the script is something else, or is compound — see `SHELL_OPERATORS`.
fn detect_script_runner(script: &str) -> Option<(ScriptRunner, Vec<String>)> {
    if SHELL_OPERATORS.iter().any(|op| script.contains(op)) {
        return None;
    }

    let mut tokens = script.split_whitespace();
    let runner = match tokens.next()? {
        "jest" => ScriptRunner::Jest,
        "vitest" => ScriptRunner::Vitest,
        _ => return None,
    };

    Some((runner, tokens.map(str::to_string).collect()))
}

pub fn run(args: &[String], verbose: u8, skip_env: bool) -> Result<i32> {
    // Determine if this is "npm run <script>" or another npm subcommand (install, list, etc.)
    // Only inject "run" when args look like a script name, not a known npm subcommand.
    let first_arg = args.first().map(|s| s.as_str());
    let is_run_explicit = first_arg == Some("run");
    let is_npm_subcommand = first_arg
        .map(|a| NPM_SUBCOMMANDS.contains(&a) || a.starts_with('-'))
        .unwrap_or(false);

    let mut effective_args: Vec<String> = Vec::with_capacity(args.len() + 1);
    if is_run_explicit || is_npm_subcommand {
        effective_args.extend_from_slice(args);
    } else {
        // "rtk npm build" → "npm run build" (assume script name)
        effective_args.push("run".to_string());
        effective_args.extend_from_slice(args);
    }

    // `npm run test` on a suite RTK already knows how to compact: send it to that
    // filter instead of npm's generic one. Measured on NEXOS/frontend, whose script
    // is plain `jest`: 620,114 bytes through npm, 3,601 through the jest filter.
    //
    // Skipped when --skip-env is set, because the dedicated path builds its own
    // command and would drop SKIP_ENV_VALIDATION without saying so.
    if !skip_env {
        if let Some(routed) = route_to_test_filter(&effective_args, verbose) {
            return routed;
        }
    }

    run_filtered("npm", &effective_args, verbose, skip_env)
}

/// Route `run <script> [args…]` to a dedicated test-runner filter when the script
/// is exactly that runner. `None` leaves the call on the ordinary npm path.
fn route_to_test_filter(effective_args: &[String], verbose: u8) -> Option<Result<i32>> {
    let (first, rest) = effective_args.split_first()?;
    if first != "run" {
        return None;
    }

    let (script_name, user_args) = rest.split_first()?;
    let (runner, mut runner_args) = detect_script_runner(&script_body(script_name)?)?;

    // npm's `--` only separates script args from npm's own; the runner never sees it.
    runner_args.extend(user_args.iter().filter(|a| *a != "--").cloned());

    let command = match runner {
        ScriptRunner::Jest => Commands::Jest {
            args: runner_args.clone(),
        },
        ScriptRunner::Vitest => Commands::Vitest {
            args: runner_args.clone(),
        },
    };

    let raw_label = format!("npm {}", effective_args.join(" "));
    let rtk_label = format!("rtk {}", raw_label);
    Some(vitest_cmd::run_test(
        &command,
        &runner_args,
        verbose,
        Some((&raw_label, &rtk_label)),
    ))
}

/// Run an npx tool through the same filtered pipeline as `npm`.
///
/// Used for unrouted tools in the `Commands::Npx` fallback so that
/// `rtk npx cowsay hello` dispatches to `npx`, not `npm`. Honors `--skip-env`
/// the same way `run` does.
pub fn exec(args: &[String], verbose: u8, skip_env: bool) -> Result<i32> {
    run_filtered("npx", args, verbose, skip_env)
}

/// Shared command-execution path for `run` (npm) and `exec` (npx).
///
/// Builds the resolved command, appends args, applies `SKIP_ENV_VALIDATION`,
/// emits the verbose log line, and routes through `runner::run_filtered` with
/// the npm output filter.
fn run_filtered(name: &str, args: &[String], verbose: u8, skip_env: bool) -> Result<i32> {
    let mut cmd = resolved_command(name);
    for arg in args {
        cmd.arg(arg);
    }

    if skip_env {
        cmd.env("SKIP_ENV_VALIDATION", "1");
    }

    let args_display = args.join(" ");
    if verbose > 0 {
        eprintln!("Running: {} {}", name, args_display);
    }

    runner::run_filtered(
        cmd,
        name,
        &args_display,
        filter_npm_output,
        runner::RunOptions::default(),
    )
}

/// Filter npm run output - strip boilerplate, progress bars, npm WARN
fn filter_npm_output(output: &str) -> String {
    let mut result = Vec::new();

    for line in output.lines() {
        // Skip npm boilerplate
        if line.starts_with('>') && line.contains('@') {
            continue;
        }
        // Skip npm lifecycle scripts
        if line.trim_start().starts_with("npm WARN") {
            continue;
        }
        if line.trim_start().starts_with("npm notice") {
            continue;
        }
        // Skip progress indicators
        if line.contains("⸩") || line.contains("⸨") || line.contains("...") && line.len() < 10 {
            continue;
        }
        // Skip empty lines
        if line.trim().is_empty() {
            continue;
        }

        result.push(line.to_string());
    }

    if result.is_empty() {
        "ok".to_string()
    } else {
        result.join("\n")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_detect_script_runner_plain_runners() {
        assert_eq!(
            detect_script_runner("jest"),
            Some((ScriptRunner::Jest, vec![]))
        );
        assert_eq!(
            detect_script_runner("vitest run"),
            Some((ScriptRunner::Vitest, vec!["run".to_string()]))
        );
        assert_eq!(
            detect_script_runner("jest --ci --coverage"),
            Some((
                ScriptRunner::Jest,
                vec!["--ci".to_string(), "--coverage".to_string()]
            ))
        );
    }

    #[test]
    fn test_detect_script_runner_refuses_compound_scripts() {
        // The dedicated filter execs the runner directly, so a compound script
        // must stay on the npm path or the rest of it silently never runs.
        for script in [
            "vitest run && node --test tests/*.test.mjs",
            "jest || echo failed",
            "jest; echo done",
            "jest | tee out.log",
            "jest > out.log",
            "$(which jest)",
        ] {
            assert_eq!(detect_script_runner(script), None, "script: {}", script);
        }
    }

    #[test]
    fn test_detect_script_runner_ignores_other_tools() {
        for script in ["next build", "tsc --noEmit", "mocha", "node --test", ""] {
            assert_eq!(detect_script_runner(script), None, "script: {}", script);
        }
    }

    #[test]
    fn test_route_to_test_filter_requires_run_subcommand() {
        // Guards the shape of the call, not the filesystem: "install" is not "run",
        // so routing must decline before it ever looks for a package.json.
        let args = vec!["install".to_string(), "jest".to_string()];
        assert!(route_to_test_filter(&args, 0).is_none());
        assert!(route_to_test_filter(&["run".to_string()], 0).is_none());
        assert!(route_to_test_filter(&[], 0).is_none());
    }

    #[test]
    fn test_filter_npm_output() {
        let output = r#"
> project@1.0.0 build
> next build

npm WARN deprecated inflight@1.0.6: This module is not supported
npm notice

   Creating an optimized production build...
   ✓ Build completed
"#;
        let result = filter_npm_output(output);
        assert!(!result.contains("npm WARN"));
        assert!(!result.contains("npm notice"));
        assert!(!result.contains("> project@"));
        assert!(result.contains("Build completed"));
    }

    #[test]
    fn test_npm_subcommand_routing() {
        // Uses the shared NPM_SUBCOMMANDS constant — no drift between prod and test
        fn needs_run_injection(args: &[&str]) -> bool {
            let first = args.first().copied();
            let is_run_explicit = first == Some("run");
            let is_subcommand = first
                .map(|a| NPM_SUBCOMMANDS.contains(&a) || a.starts_with('-'))
                .unwrap_or(false);
            !is_run_explicit && !is_subcommand
        }

        // Known subcommands should NOT get "run" injected
        for subcmd in NPM_SUBCOMMANDS {
            assert!(
                !needs_run_injection(&[subcmd]),
                "'npm {}' should NOT inject 'run'",
                subcmd
            );
        }

        // Script names SHOULD get "run" injected
        for script in &["build", "dev", "lint", "typecheck", "deploy"] {
            assert!(
                needs_run_injection(&[script]),
                "'npm {}' SHOULD inject 'run'",
                script
            );
        }

        // Flags should NOT get "run" injected
        assert!(!needs_run_injection(&["--version"]));
        assert!(!needs_run_injection(&["-h"]));

        // Explicit "run" should NOT inject another "run"
        assert!(!needs_run_injection(&["run", "build"]));
    }

    #[test]
    fn test_filter_npm_output_empty() {
        let output = "\n\n\n";
        let result = filter_npm_output(output);
        assert_eq!(result, "ok");
    }
}
