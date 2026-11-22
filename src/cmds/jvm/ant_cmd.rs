//! Filters Apache Ant (`ant`) command output — strips target chatter, task
//! announcements, and buildfile location noise while preserving compile errors,
//! BUILD SUCCESSFUL/FAILED, and the total-time summary.

use crate::core::arg_tokenizer::{Dialect, TokenKind, ValueSpec, tokenize_grammar};
use crate::core::runner::{self, RunOptions};
use crate::core::shell::display_args;
use crate::core::truncate::CAP_ERRORS;
use crate::core::utils::{resolved_command, strip_ansi, truncate};
use anyhow::Result;
use regex::Regex;
use std::ffi::OsString;
use std::process::Command;
use std::sync::LazyLock;

// ── Shared regex patterns ─────────────────────────────────────────────────────

// Lines we always preserve regardless of other rules.
static RE_KEEP_BUILD_RESULT: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^BUILD (SUCCESSFUL|FAILED)").unwrap());
static RE_KEEP_ERROR: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(?i)error:").unwrap());
static RE_KEEP_FAILED: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(?i)failed:?").unwrap());
static RE_KEEP_TOTAL_TIME: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^Total time:").unwrap());
// [javac] lines with file:line compile errors, e.g. "/path/Foo.java:42: error:"
static RE_KEEP_JAVAC_ERROR: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^\s+\[javac\].*:\d+").unwrap());

// Lines we strip (noise).
static RE_STRIP_BUILDFILE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^Buildfile:").unwrap());
// Target labels: "compile:", "init:", and internal hyphen-prefixed targets
// like "-check-git-state:" / "-ivy-fail-disallowed-ivy-version:".
static RE_STRIP_TARGET: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^-?[a-z][a-zA-Z0-9_.-]*:$").unwrap());
// Generic harmless task chatter (echo, delete, mkdir, copy, move, ivy
// resolution noise, etc.). [javac]/[junit]/[testng] are handled separately
// so diagnostics and test results are never swept up here.
static RE_STRIP_TASK_CHATTER: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r"^\s*\[(echo|delete|mkdir|copy|move|propertyfile|fixcrlf|loadresource|ivy:[a-z]+)\]",
    )
    .unwrap()
});
// [javac] informational chatter only — never strip lines that contain
// diagnostic context (source snippets, carets, symbol/location, error count
// summaries). Stripping all `[javac]` lines drops the user-actionable
// continuation lines that follow a `file:line: error:` header.
static RE_STRIP_JAVAC_INFO: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r"^\s+\[javac\]\s+(?:Compiling\b|Note:|Compiled\b|warning:\s*\[options\]|Usage:|use --help\b)",
    )
    .unwrap()
});

// ── Argument grammar ──────────────────────────────────────────────────────────

/// Ant's CLI takes whole single-dash words (`-buildfile`, `-ntp`-style
/// clustering does not exist), like Maven's commons-cli.
const ANT_DIALECT: Dialect = Dialect::CommonsCli;

/// Options declared `<arg>`-taking by `ant -h`, so their values are never
/// mistaken for build targets (e.g. `-f build.xml` must not classify
/// `build.xml` as a target).
fn ant_takes_value(_kind: TokenKind, name: &str) -> Option<ValueSpec> {
    matches!(
        name,
        "lib"
            | "logfile"
            | "l"
            | "logger"
            | "listener"
            | "D"
            | "buildfile"
            | "file"
            | "f"
            | "propertyfile"
            | "inputhandler"
            | "find"
            | "s"
            | "nice"
            | "main"
    )
    .then(ValueSpec::value)
}

/// The build targets in `args`, in order (flags and their values excluded).
fn targets(args: &[String]) -> Vec<&str> {
    tokenize_grammar(args, &ant_takes_value, ANT_DIALECT)
        .iter()
        .filter(|t| t.is_free_positional())
        .map(|t| t.text)
        .collect()
}

/// The tee label for an invocation: the last target decides (`clean test` is a
/// test run). Bare `ant` runs the buildfile's default target → build.
fn tee_label(args: &[String]) -> &'static str {
    let last = targets(args).last().map(|t| t.to_lowercase());
    match last.as_deref() {
        Some(t) if t.contains("test") || t.contains("junit") => "ant_test",
        _ => "ant_build",
    }
}

/// Returns the Ant executable: prefers `./antw` (wrapper), falls back to `ant`.
fn ant_binary() -> &'static str {
    if cfg!(windows) {
        if std::path::Path::new(".\\antw.bat").exists() {
            ".\\antw.bat"
        } else {
            "ant"
        }
    } else if std::path::Path::new("./antw").exists() {
        "./antw"
    } else {
        "ant"
    }
}

/// Builds an Ant `Command`. Local wrappers are passed as string literals
/// (semgrep `dynamic-command-execution`); the system binary resolves via
/// `resolved_command` for PATHEXT support on Windows — matches gradlew/mvn.
fn new_ant_command(args: &[String]) -> Command {
    let mut cmd = if cfg!(windows) {
        if std::path::Path::new(".\\antw.bat").exists() {
            Command::new(".\\antw.bat")
        } else {
            resolved_command("ant")
        }
    } else if std::path::Path::new("./antw").exists() {
        Command::new("./antw")
    } else {
        resolved_command("ant")
    };
    cmd.args(args);
    cmd
}

pub fn run(args: &[String], verbose: u8) -> Result<i32> {
    // Verbose/diagnostic/help flags bypass filtering — user wants full output,
    // and `-projecthelp`-style listings would be mangled by the target-label strip.
    if args.iter().any(|a| {
        matches!(
            a.as_str(),
            "-v" | "-verbose"
                | "-d"
                | "-debug"
                | "-p"
                | "-projecthelp"
                | "-h"
                | "-help"
                | "-version"
                | "-diagnostics"
        )
    }) {
        let osargs: Vec<OsString> = args.iter().map(OsString::from).collect();
        return runner::run_passthrough(ant_binary(), &osargs, verbose);
    }

    let tee = tee_label(args);
    let filter = if tee == "ant_test" {
        |raw: &str| filter_ant_test(&strip_ansi(raw))
    } else {
        |raw: &str| filter_ant_build(&strip_ansi(raw))
    };

    runner::run_filtered(
        new_ant_command(args),
        ant_binary(),
        &display_args(args),
        filter,
        RunOptions::with_tee(tee),
    )
}

/// Build/compile/clean/package/install filter — strips target announcements and
/// task chatter, keeps errors and BUILD banner.
pub fn filter_ant_build(output: &str) -> String {
    let mut kept: Vec<String> = Vec::new();
    // Cap the `[javac]` diagnostic flood: a failed compile can emit hundreds of
    // multi-line error blocks. Keep the first CAP_ERRORS lines (the leading
    // errors with their full source/caret/symbol context), summarise the rest,
    // and always preserve the BUILD banner that follows.
    let mut javac_lines = 0usize;
    let mut dropped = 0usize;
    let mut marker_pos: Option<usize> = None;

    for line in output.lines() {
        if line.trim().is_empty() {
            continue;
        }

        let is_keep = RE_KEEP_BUILD_RESULT.is_match(line)
            || RE_KEEP_TOTAL_TIME.is_match(line)
            || RE_KEEP_JAVAC_ERROR.is_match(line)
            || RE_KEEP_ERROR.is_match(line)
            || RE_KEEP_FAILED.is_match(line);

        // Strip noise unless the line is an error/result we always keep.
        if !is_keep
            && (RE_STRIP_BUILDFILE.is_match(line)
                || RE_STRIP_TARGET.is_match(line)
                || RE_STRIP_TASK_CHATTER.is_match(line)
                || RE_STRIP_JAVAC_INFO.is_match(line))
        {
            continue;
        }

        // Cap [javac] diagnostic lines; non-javac lines (BUILD banner, the
        // build.xml failure trail, Total time) are never capped.
        if line.trim_start().starts_with("[javac]") {
            javac_lines += 1;
            if javac_lines > CAP_ERRORS {
                marker_pos.get_or_insert(kept.len());
                dropped += 1;
                continue;
            }
        }

        kept.push(truncate(line.trim_end(), 240).to_string());
    }

    if let Some(pos) = marker_pos {
        kept.insert(pos, format!("    … +{} more javac lines", dropped));
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

    fn s(args: &[&str]) -> Vec<String> {
        args.iter().map(|a| a.to_string()).collect()
    }

    // ── TARGET / TEE DETECTION ───────────────────────────────────────────────

    #[test]
    fn test_tee_label_test_target() {
        assert_eq!(tee_label(&s(&["test"])), "ant_test");
        assert_eq!(tee_label(&s(&["junit-all"])), "ant_test");
        assert_eq!(tee_label(&s(&["clean", "test"])), "ant_test");
    }

    #[test]
    fn test_tee_label_build_targets() {
        assert_eq!(tee_label(&s(&["build"])), "ant_build");
        assert_eq!(tee_label(&s(&["clean"])), "ant_build");
        assert_eq!(tee_label(&s(&["compile"])), "ant_build");
        assert_eq!(tee_label(&s(&["package"])), "ant_build");
        // Bare `ant` (default target) → build
        assert_eq!(tee_label(&s(&[])), "ant_build");
    }

    #[test]
    fn test_flag_value_is_not_a_target() {
        // `-f test-build.xml compile` must not classify as a test run:
        // `test-build.xml` is the -f value, the target is `compile`.
        assert_eq!(
            tee_label(&s(&["-f", "test-build.xml", "compile"])),
            "ant_build"
        );
        assert_eq!(tee_label(&s(&["-Dsuite=test", "build"])), "ant_build");
    }

    // ── BUILD FILTER ─────────────────────────────────────────────────────────

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
        assert!(
            !filtered.contains("Buildfile:"),
            "Buildfile line should be stripped"
        );
        assert!(
            !filtered.contains("clean:"),
            "target label should be stripped"
        );
        assert!(
            !filtered.contains("[delete]"),
            "delete task should be stripped"
        );
        assert!(
            !filtered.contains("[mkdir]"),
            "mkdir task should be stripped"
        );
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
        assert!(
            !filtered.contains("Buildfile:"),
            "Buildfile line should be stripped"
        );
    }

    #[test]
    fn test_empty_input_passes_through() {
        assert_eq!(filter_ant_build(""), "ant: ok");
        assert_eq!(filter_ant_build("   \n\n   "), "ant: ok");
    }

    #[test]
    fn test_strips_javac_usage_help_keeps_error() {
        let output = "\
compile:
    [javac] Compiling 22 source files to /tmp/out
    [javac] error: release version 7 not supported
    [javac] Usage: javac <options> <source files>
    [javac] use --help for a list of possible options

BUILD FAILED
/path/build.xml:764: Compile failed; see the compiler error output for details.

Total time: 0 seconds
";
        let filtered = filter_ant_build(output);
        assert!(
            filtered.contains("error: release version 7 not supported"),
            "real javac error must be preserved, got:\n{}",
            filtered
        );
        assert!(
            !filtered.contains("Usage: javac"),
            "javac usage help should be stripped, got:\n{}",
            filtered
        );
        assert!(
            !filtered.contains("use --help"),
            "javac --help hint should be stripped, got:\n{}",
            filtered
        );
        assert!(filtered.contains("BUILD FAILED"));
        assert!(filtered.contains("Total time:"));
    }

    #[test]
    fn test_strips_hyphen_targets_and_ivy_chatter() {
        // Ivy/Ant builds (e.g. lucene-solr) emit hyphen-prefixed internal
        // targets and repeated ivy/loadresource task chatter.
        // Note: ivy/loadresource chatter is often emitted at column 0 (no
        // leading indent), unlike [javac]; the strip pattern must allow both.
        let output = "\
-check-git-state:
-ivy-fail-disallowed-ivy-version:
[loadresource] Do not set property disallowed.ivy.jars.list as its length is 0.
[ivy:configure] :: Apache Ivy 2.4.0 :: http://ant.apache.org/ivy/ ::
[ivy:retrieve] :: retrieving :: org.apache#lucene
compile:
    [javac] Compiling 200 source files

BUILD SUCCESSFUL
Total time: 12 seconds
";
        let filtered = filter_ant_build(output);
        assert!(
            !filtered.contains("-check-git-state:"),
            "hyphen-prefixed target should be stripped, got:\n{}",
            filtered
        );
        assert!(
            !filtered.contains("-ivy-fail-disallowed"),
            "hyphen-prefixed ivy target should be stripped, got:\n{}",
            filtered
        );
        assert!(
            !filtered.contains("[ivy:configure]") && !filtered.contains("[ivy:retrieve]"),
            "ivy task chatter should be stripped, got:\n{}",
            filtered
        );
        assert!(
            !filtered.contains("[loadresource]"),
            "loadresource chatter should be stripped, got:\n{}",
            filtered
        );
        assert!(filtered.contains("BUILD SUCCESSFUL"));
        assert!(filtered.contains("Total time:"));
    }

    #[test]
    fn test_caps_javac_error_flood_keeps_banner() {
        let mut input = String::from("compile:\n");
        // 60 distinct compile errors, each with a caret context line (120 javac lines).
        for i in 0..60 {
            input.push_str(&format!(
                "    [javac] /path/File{i}.java:{i}: error: cannot find symbol\n    [javac]            ^\n"
            ));
        }
        input.push_str("\nBUILD FAILED\n/path/build.xml:52: Compile failed; see details.\n\nTotal time: 1 second\n");

        let filtered = filter_ant_build(&input);
        let javac_kept = filtered
            .lines()
            .filter(|l| l.trim_start().starts_with("[javac]"))
            .count();
        assert!(
            javac_kept <= CAP_ERRORS,
            "javac flood must be capped at {CAP_ERRORS}, kept {javac_kept}:\n{filtered}"
        );
        assert!(
            filtered.contains("more javac lines"),
            "must summarise dropped javac lines, got:\n{}",
            filtered
        );
        // First error retained with full context; banner never dropped.
        assert!(filtered.contains("/path/File0.java:0: error:"));
        assert!(filtered.contains("BUILD FAILED"));
        assert!(filtered.contains("Total time:"));
    }

    #[test]
    fn test_token_savings_floor() {
        fn count_tokens(text: &str) -> usize {
            text.split_whitespace().count()
        }
        let input = "\
Buildfile: /path/to/build.xml

-check-env:
init:
    [mkdir] Created dir: /tmp/build/classes
    [copy] Copying 12 files to /tmp/build/resources
    [echo] Building project example

compile:
    [javac] Compiling 47 source files to /tmp/build/classes

jar:
    [copy] Copying 3 files to /tmp/build/jar
    [echo] Packaging example.jar

BUILD SUCCESSFUL

Total time: 4 seconds
";
        let output = filter_ant_build(input);
        let savings = 100.0 - (count_tokens(&output) as f64 / count_tokens(input) as f64 * 100.0);
        assert!(
            savings >= 20.0,
            "Expected ≥20% savings, got {:.1}%",
            savings
        );
    }
}
