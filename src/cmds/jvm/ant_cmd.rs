//! Filters Apache Ant (`ant`) command output — strips target chatter, task
//! announcements, and buildfile location noise while preserving compile errors,
//! BUILD SUCCESSFUL/FAILED, and the total-time summary.

use crate::core::runner;
use crate::core::utils::{exit_code_from_output, resolved_build_command, strip_ansi, truncate};
use anyhow::{Context, Result};
use lazy_static::lazy_static;
use regex::Regex;
use std::ffi::OsString;

/// Native Ant targets that rtk filters directly.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AntCommand {
    Build,
    Clean,
    Test,
    Compile,
    Package,
    Install,
}

impl AntCommand {
    fn target(self) -> &'static str {
        match self {
            AntCommand::Build => "build",
            AntCommand::Clean => "clean",
            AntCommand::Test => "test",
            AntCommand::Compile => "compile",
            AntCommand::Package => "package",
            AntCommand::Install => "install",
        }
    }

    fn label(self) -> &'static str {
        match self {
            AntCommand::Build => "ant build",
            AntCommand::Clean => "ant clean",
            AntCommand::Test => "ant test",
            AntCommand::Compile => "ant compile",
            AntCommand::Package => "ant package",
            AntCommand::Install => "ant install",
        }
    }

    fn tee_label(self) -> &'static str {
        match self {
            AntCommand::Build => "ant_build",
            AntCommand::Clean => "ant_clean",
            AntCommand::Test => "ant_test",
            AntCommand::Compile => "ant_compile",
            AntCommand::Package => "ant_package",
            AntCommand::Install => "ant_install",
        }
    }
}

lazy_static! {
    // Lines we always preserve regardless of other rules.
    static ref RE_KEEP_BUILD_RESULT: Regex =
        Regex::new(r"^BUILD (SUCCESSFUL|FAILED)").unwrap();
    static ref RE_KEEP_ERROR: Regex =
        Regex::new(r"(?i)error:").unwrap();
    static ref RE_KEEP_FAILED: Regex =
        Regex::new(r"(?i)failed:?").unwrap();
    static ref RE_KEEP_TOTAL_TIME: Regex =
        Regex::new(r"^Total time:").unwrap();
    // [javac] lines with file:line compile errors, e.g. "/path/Foo.java:42: error:"
    static ref RE_KEEP_JAVAC_ERROR: Regex =
        Regex::new(r"^\s+\[javac\].*:\d+").unwrap();

    // Lines we strip (noise).
    static ref RE_STRIP_BUILDFILE: Regex =
        Regex::new(r"^Buildfile:").unwrap();
    // Lowercase target labels: "compile:", "init:", etc.
    static ref RE_STRIP_TARGET: Regex =
        Regex::new(r"^[a-z][a-zA-Z0-9_-]+:$").unwrap();
    // Generic harmless task chatter (echo, delete, mkdir, copy, move, etc.).
    static ref RE_STRIP_TASK_CHATTER: Regex =
        Regex::new(r"^\s+\[(echo|delete|mkdir|copy|move|propertyfile|fixcrlf)\]").unwrap();
    // [javac] informational chatter only — never strip lines that contain
    // diagnostic context (source snippets, carets, symbol/location, error count
    // summaries). Stripping all `[javac]` lines drops the user-actionable
    // continuation lines that follow a `file:line: error:` header.
    static ref RE_STRIP_JAVAC_INFO: Regex =
        Regex::new(r"^\s+\[javac\]\s+(?:Compiling\b|Note:|Compiled\b|warning:\s*\[options\])").unwrap();
}

/// Execute a known ant target with compact filtering.
pub fn run(cmd: AntCommand, args: &[String], verbose: u8) -> Result<i32> {
    let mut command = resolved_build_command("ant");
    command.arg(cmd.target());
    for arg in args {
        command.arg(arg);
    }

    if verbose > 0 {
        eprintln!("Running: {} {}", cmd.label(), args.join(" "));
    }

    let tee = cmd.tee_label();
    let filter: fn(&str) -> String = match cmd {
        AntCommand::Test => |raw| filter_ant_test(&strip_ansi(raw)),
        _ => |raw| filter_ant_build(&strip_ansi(raw)),
    };
    let filter = move |raw: &str| filter(raw);

    runner::run_filtered(
        command,
        cmd.label(),
        &args.join(" "),
        filter,
        runner::RunOptions::with_tee(tee),
    )
}

/// Passthrough for any `ant` invocation rtk doesn't specialise.
pub fn run_passthrough(args: &[OsString], verbose: u8) -> Result<i32> {
    if args.is_empty() {
        anyhow::bail!("ant: no target specified");
    }

    let mut command = resolved_build_command("ant");
    for arg in args {
        command.arg(arg);
    }

    if verbose > 0 {
        let joined = args
            .iter()
            .map(|a| a.to_string_lossy().into_owned())
            .collect::<Vec<_>>()
            .join(" ");
        eprintln!("Running: ant {}", joined);
    }

    let output = command.output().context("Failed to run ant")?;
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    let raw = format!("{}\n{}", stdout, stderr);

    let filtered = filter_ant_build(&strip_ansi(&raw));
    println!("{}", filtered);

    Ok(exit_code_from_output(&output, "ant"))
}

/// Build/compile/clean/package/install filter — strips target announcements and
/// task chatter, keeps errors and BUILD banner.
pub fn filter_ant_build(output: &str) -> String {
    let mut kept: Vec<String> = Vec::new();

    for line in output.lines() {
        if line.trim().is_empty() {
            continue;
        }

        // Always preserve these.
        if RE_KEEP_BUILD_RESULT.is_match(line)
            || RE_KEEP_TOTAL_TIME.is_match(line)
            || RE_KEEP_JAVAC_ERROR.is_match(line)
            || RE_KEEP_ERROR.is_match(line)
            || RE_KEEP_FAILED.is_match(line)
        {
            kept.push(truncate(line.trim_end(), 240).to_string());
            continue;
        }

        // Strip these patterns.
        if RE_STRIP_BUILDFILE.is_match(line)
            || RE_STRIP_TARGET.is_match(line)
            || RE_STRIP_TASK_CHATTER.is_match(line)
            || RE_STRIP_JAVAC_INFO.is_match(line)
        {
            continue;
        }

        kept.push(truncate(line.trim_end(), 240).to_string());
    }

    if kept.is_empty() {
        return "ant: ok".to_string();
    }

    kept.join("\n")
}

/// Test filter — same as build filter (Ant test output structure is similar).
pub fn filter_ant_test(output: &str) -> String {
    filter_ant_build(output)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_successful_build_collapses_chatter_keeps_banner() {
        let output = "\
Buildfile: /path/to/build.xml

clean:
   [delete] Deleting directory /tmp/build

init:
    [mkdir] Created dir: /tmp/build/classes

compile:
    [javac] Compiling 47 source files to /tmp/build/classes

BUILD SUCCESSFUL

Total time: 4 seconds
";
        let filtered = filter_ant_build(output);
        assert!(
            filtered.contains("BUILD SUCCESSFUL"),
            "BUILD SUCCESSFUL must be preserved, got:\n{}",
            filtered
        );
        assert!(
            filtered.contains("Total time:"),
            "Total time must be preserved, got:\n{}",
            filtered
        );
        assert!(!filtered.contains("Buildfile:"), "Buildfile line should be stripped");
        assert!(!filtered.contains("clean:"), "target label should be stripped");
        assert!(!filtered.contains("[delete]"), "delete task should be stripped");
        assert!(!filtered.contains("[mkdir]"), "mkdir task should be stripped");
        assert!(
            !filtered.contains("Compiling 47"),
            "plain [javac] chatter without errors should be stripped"
        );
    }

    #[test]
    fn test_failed_compile_preserves_error_and_banner() {
        let output = "\
Buildfile: /path/to/build.xml

compile:
    [javac] Compiling 47 source files to /tmp/build/classes
    [javac] /path/Foo.java:42: error: cannot find symbol
    [javac] /path/Bar.java:10: error: incompatible types

BUILD FAILED
/path/build.xml:35: Compile failed; see the compiler error output for details.

Total time: 4 seconds
";
        let filtered = filter_ant_build(output);
        assert!(
            filtered.contains("BUILD FAILED"),
            "BUILD FAILED must be preserved, got:\n{}",
            filtered
        );
        assert!(
            filtered.contains("/path/Foo.java:42: error:"),
            "compile error line must be preserved, got:\n{}",
            filtered
        );
        assert!(
            filtered.contains("/path/Bar.java:10: error:"),
            "second compile error must be preserved, got:\n{}",
            filtered
        );
        assert!(
            filtered.contains("Total time:"),
            "Total time must be preserved, got:\n{}",
            filtered
        );
        assert!(!filtered.contains("Buildfile:"), "Buildfile line should be stripped");
    }

    #[test]
    fn test_empty_input_passes_through() {
        assert_eq!(filter_ant_build(""), "ant: ok");
        assert_eq!(filter_ant_build("   \n\n   "), "ant: ok");
    }
}
