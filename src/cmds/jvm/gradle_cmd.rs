//! Filters Gradle command output — build, test, assemble, clean, check, bootRun.
//!
//! Strips progress lines, daemon/configure chatter and jansi/SLF4J/native-platform
//! warnings, but preserves failure markers, deprecation warnings, test failures,
//! compiler errors and the final `BUILD SUCCESSFUL`/`BUILD FAILED` lines.

use crate::cmds::jvm::line_dedup::dedupe_repeated_lines;
use crate::cmds::jvm::stack_dedup::dedupe_stack_traces;
use crate::cmds::jvm::stack_trim::trim_stack_noise;
use crate::core::runner;
use crate::core::tracking;
use crate::core::utils::{exit_code_from_output, resolved_build_command, truncate};
use anyhow::{Context, Result};
use lazy_static::lazy_static;
use regex::Regex;
use std::ffi::OsString;

/// Supported `rtk gradle` subcommands.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GradleCommand {
    Build,
    Test,
    Assemble,
    Clean,
    Check,
    BootRun,
}

impl GradleCommand {
    fn as_task(self) -> &'static str {
        match self {
            GradleCommand::Build => "build",
            GradleCommand::Test => "test",
            GradleCommand::Assemble => "assemble",
            GradleCommand::Clean => "clean",
            GradleCommand::Check => "check",
            GradleCommand::BootRun => "bootRun",
        }
    }

    fn label(self) -> &'static str {
        match self {
            GradleCommand::Build => "gradle build",
            GradleCommand::Test => "gradle test",
            GradleCommand::Assemble => "gradle assemble",
            GradleCommand::Clean => "gradle clean",
            GradleCommand::Check => "gradle check",
            GradleCommand::BootRun => "gradle bootRun",
        }
    }

    fn tee_label(self) -> &'static str {
        match self {
            GradleCommand::Build => "gradle_build",
            GradleCommand::Test => "gradle_test",
            GradleCommand::Assemble => "gradle_assemble",
            GradleCommand::Clean => "gradle_clean",
            GradleCommand::Check => "gradle_check",
            GradleCommand::BootRun => "gradle_bootrun",
        }
    }
}

lazy_static! {
    // Progress / transient lines that appear under carriage returns.
    static ref RE_PROGRESS_TASKS: Regex = Regex::new(r"<\d+/\d+\s+tasks>").unwrap();
    static ref RE_PROGRESS_EXECUTING: Regex = Regex::new(r"\b\d+%\s+EXECUTING").unwrap();
    static ref RE_PROGRESS_CONFIGURING: Regex = Regex::new(r"\b\d+%\s+CONFIGURING").unwrap();
    static ref RE_PROGRESS_INITIALIZING: Regex = Regex::new(r"\b\d+%\s+INITIALIZING").unwrap();
    static ref RE_PROGRESS_WAITING: Regex = Regex::new(r"\b\d+%\s+WAITING").unwrap();

    // Task announcements (strip unless the task line marks a failure).
    static ref RE_TASK_ANNOUNCE: Regex = Regex::new(r"^> Task :").unwrap();
    static ref RE_TASK_FAILED: Regex = Regex::new(r"^> Task :.*\bFAILED\b").unwrap();

    // Configure / daemon / welcome chatter.
    static ref RE_CONFIGURE_PROJECT: Regex = Regex::new(r"^Configure project").unwrap();
    static ref RE_CONFIGURATION_ON_DEMAND: Regex =
        Regex::new(r"^Configuration on demand").unwrap();
    static ref RE_WELCOME: Regex = Regex::new(r"^Welcome to Gradle").unwrap();
    static ref RE_DAEMON_STOPPED: Regex = Regex::new(r"^Daemon will be stopped").unwrap();
    static ref RE_DAEMON_STARTING: Regex = Regex::new(r"^Starting a Gradle Daemon").unwrap();

    // jansi / SLF4J / native-platform restricted-access warnings.
    static ref RE_WARN_RESTRICTED_METHOD: Regex =
        Regex::new(r"^WARNING: A restricted method").unwrap();
    static ref RE_WARN_SYSTEM_LOAD: Regex = Regex::new(
        r"^WARNING: java\.lang\.System::load has been called by net\.rubygrapefruit\.platform"
    )
    .unwrap();
    static ref RE_WARN_NATIVE_ACCESS: Regex =
        Regex::new(r"^WARNING: Use --enable-native-access").unwrap();
    static ref RE_WARN_RESTRICTED_BLOCKED: Regex =
        Regex::new(r"^WARNING: Restricted methods will be blocked").unwrap();

    // Positive markers we always want to keep.
    static ref RE_FAILURE_HEADER: Regex = Regex::new(r"^FAILURE:").unwrap();
    static ref RE_BUILD_RESULT: Regex =
        Regex::new(r"^BUILD (SUCCESSFUL|FAILED)").unwrap();
    static ref RE_DEPRECATED: Regex =
        Regex::new(r"Deprecated Gradle features were used").unwrap();
    static ref RE_WHERE: Regex = Regex::new(r"^Where:").unwrap();
    static ref RE_WHAT_WENT_WRONG: Regex = Regex::new(r"^\* What went wrong:|^What went wrong:").unwrap();
    static ref RE_TRY: Regex = Regex::new(r"^\* Try:|^Try:").unwrap();

    // Loose match for classic Java compilation errors, e.g.
    // "/src/main/java/Foo.java:42: error: cannot find symbol".
    static ref RE_JAVA_COMPILE_ERROR: Regex =
        Regex::new(r"\.(java|kt|groovy|scala):\d+").unwrap();
}

/// Inject `--console=plain` (kills live progress bars + ANSI escapes) unless the
/// user already specified a console mode. Supported on all maintained Gradle
/// versions (5.0+).
fn inject_quiet_flags(command: &mut std::process::Command, args: &[String]) {
    let already_has_console = args
        .iter()
        .any(|a| a == "--console" || a.starts_with("--console="));
    if !already_has_console {
        command.arg("--console=plain");
    }
}

/// Same as [`inject_quiet_flags`] for OsString-based passthrough args.
fn inject_quiet_flags_os(command: &mut std::process::Command, args: &[OsString]) {
    let already_has_console = args.iter().any(|a| {
        let s = a.to_string_lossy();
        s == "--console" || s.starts_with("--console=")
    });
    if !already_has_console {
        command.arg("--console=plain");
    }
}

/// Run a known gradle subcommand with filtered output.
pub fn run(cmd: GradleCommand, args: &[String], verbose: u8) -> Result<i32> {
    let mut command = resolved_build_command("gradle");
    inject_quiet_flags(&mut command, args);
    command.arg(cmd.as_task());

    for arg in args {
        command.arg(arg);
    }

    if verbose > 0 {
        eprintln!("Running: gradle {} {}", cmd.as_task(), args.join(" "));
    }

    let filter_fn = match cmd {
        GradleCommand::Build => filter_gradle_build,
        GradleCommand::Test => filter_gradle_test,
        GradleCommand::Assemble => filter_gradle_assemble,
        GradleCommand::Clean => filter_gradle_clean,
        GradleCommand::Check => filter_gradle_check,
        GradleCommand::BootRun => filter_gradle_bootrun,
    };

    let args_str = args.join(" ");
    runner::run_filtered(
        command,
        cmd.label(),
        &args_str,
        filter_fn,
        runner::RunOptions::with_tee(cmd.tee_label()),
    )
}

/// Passthrough for any gradle subcommand we don't model explicitly.
/// Output is printed unchanged but still tracked for analytics.
pub fn run_passthrough(args: &[OsString], verbose: u8) -> Result<i32> {
    if args.is_empty() {
        anyhow::bail!("gradle: no subcommand specified");
    }

    let timer = tracking::TimedExecution::start();

    let subcommand = args[0].to_string_lossy().into_owned();
    let mut cmd = resolved_build_command("gradle");
    inject_quiet_flags_os(&mut cmd, args);
    for arg in args {
        cmd.arg(arg);
    }

    if verbose > 0 {
        eprintln!("Running: gradle {} ...", subcommand);
    }

    let output = cmd
        .output()
        .with_context(|| format!("Failed to run gradle {}", subcommand))?;

    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    let raw = format!("{}\n{}", stdout, stderr);

    print!("{}", stdout);
    eprint!("{}", stderr);

    timer.track(
        &format!("gradle {}", subcommand),
        &format!("rtk gradle {}", subcommand),
        &raw,
        &raw,
    );

    Ok(exit_code_from_output(&output, "gradle"))
}

/// Line-by-line strip for a single Gradle output stream.
fn strip_gradle_noise(output: &str) -> Vec<String> {
    // Collapse repeated JVM stack-frame blocks first, trim intra-trace noise,
    // then collapse repeated single lines (e.g. 26x "> Configure project :foo")
    // before per-line filtering so the keep/strip logic sees a compact input.
    let deduped = dedupe_repeated_lines(&trim_stack_noise(&dedupe_stack_traces(output)));
    let mut kept: Vec<String> = Vec::new();

    for raw_line in deduped.lines() {
        // Gradle overwrites progress using carriage return; each "line" may contain
        // several embedded progress frames. Take the final segment for classification.
        let line = raw_line.rsplit('\r').next().unwrap_or(raw_line);
        let trimmed = line.trim_end();

        if should_strip_line(trimmed) {
            continue;
        }

        if trimmed.trim().is_empty() {
            continue;
        }

        kept.push(trimmed.to_string());
    }

    kept
}

fn should_strip_line(line: &str) -> bool {
    // Always preserve the positive markers first.
    if RE_FAILURE_HEADER.is_match(line)
        || RE_BUILD_RESULT.is_match(line)
        || RE_DEPRECATED.is_match(line)
        || RE_WHERE.is_match(line)
        || RE_WHAT_WENT_WRONG.is_match(line)
        || RE_TRY.is_match(line)
    {
        return false;
    }

    // Task line: keep only if it carries a FAILED marker.
    if RE_TASK_ANNOUNCE.is_match(line) {
        return !RE_TASK_FAILED.is_match(line);
    }

    RE_PROGRESS_TASKS.is_match(line)
        || RE_PROGRESS_EXECUTING.is_match(line)
        || RE_PROGRESS_CONFIGURING.is_match(line)
        || RE_PROGRESS_INITIALIZING.is_match(line)
        || RE_PROGRESS_WAITING.is_match(line)
        || RE_CONFIGURE_PROJECT.is_match(line)
        || RE_CONFIGURATION_ON_DEMAND.is_match(line)
        || RE_WELCOME.is_match(line)
        || RE_DAEMON_STOPPED.is_match(line)
        || RE_DAEMON_STARTING.is_match(line)
        || RE_WARN_RESTRICTED_METHOD.is_match(line)
        || RE_WARN_SYSTEM_LOAD.is_match(line)
        || RE_WARN_NATIVE_ACCESS.is_match(line)
        || RE_WARN_RESTRICTED_BLOCKED.is_match(line)
}

fn render(lines: &[String], empty_msg: &str) -> String {
    if lines.is_empty() {
        return empty_msg.to_string();
    }

    lines
        .iter()
        .map(|l| truncate(l, 200))
        .collect::<Vec<_>>()
        .join("\n")
}

/// Filter `gradle build` output.
fn filter_gradle_build(output: &str) -> String {
    render(&strip_gradle_noise(output), "gradle build: ok")
}

/// Filter `gradle test` output. Keeps compilation errors, failed task lines,
/// `FAILURE:` blocks and test-failure stack traces.
fn filter_gradle_test(output: &str) -> String {
    render(&strip_gradle_noise(output), "gradle test: ok")
}

/// Filter `gradle assemble` output.
fn filter_gradle_assemble(output: &str) -> String {
    render(&strip_gradle_noise(output), "gradle assemble: ok")
}

/// Filter `gradle clean` output.
fn filter_gradle_clean(output: &str) -> String {
    render(&strip_gradle_noise(output), "gradle clean: ok")
}

/// Filter `gradle check` output.
fn filter_gradle_check(output: &str) -> String {
    render(&strip_gradle_noise(output), "gradle check: ok")
}

/// Filter `gradle bootRun` output. Same filtering rules; bootRun tends to
/// stream a lot of Spring log output — the filter leaves those lines alone
/// since they aren't in the strip list and mostly contain useful context.
fn filter_gradle_bootrun(output: &str) -> String {
    render(&strip_gradle_noise(output), "gradle bootRun: started")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_filter_gradle_build_strips_progress_and_configure_lines() {
        let input = "\
Starting a Gradle Daemon
Configure project :app
<2/10 tasks> 20% EXECUTING
> Task :compileJava
> Task :compileJava UP-TO-DATE
BUILD SUCCESSFUL in 12s
";
        let out = filter_gradle_build(input);
        assert!(!out.contains("Starting a Gradle Daemon"));
        assert!(!out.contains("Configure project"));
        assert!(!out.contains("20% EXECUTING"));
        assert!(!out.contains("<2/10 tasks>"));
        assert!(!out.contains("> Task :compileJava"));
        assert!(out.contains("BUILD SUCCESSFUL"));
    }

    #[test]
    fn test_filter_gradle_test_preserves_failure_markers_and_task_failed() {
        let input = "\
> Task :test
> Task :test FAILED

FAILURE: Build failed with an exception.

* Where:
Build file '/app/build.gradle'

* What went wrong:
Execution failed for task ':test'.

* Try:
> Run with --stacktrace

BUILD FAILED in 5s
";
        let out = filter_gradle_test(input);
        assert!(out.contains("> Task :test FAILED"));
        assert!(out.contains("FAILURE: Build failed"));
        assert!(out.contains("Where:"));
        assert!(out.contains("What went wrong:"));
        assert!(out.contains("Try:"));
        assert!(out.contains("BUILD FAILED"));
        // The neutral task announcement (without FAILED) must be stripped.
        assert!(!out.contains("> Task :test\n"));
    }

    #[test]
    fn test_filter_gradle_build_strips_jansi_warnings() {
        let input = "\
WARNING: A restricted method in java.lang.System has been called
WARNING: java.lang.System::load has been called by net.rubygrapefruit.platform.internal.NativeLibraryLoader
WARNING: Use --enable-native-access=ALL-UNNAMED to avoid a warning
WARNING: Restricted methods will be blocked in a future release
BUILD SUCCESSFUL in 3s
";
        let out = filter_gradle_build(input);
        assert!(!out.contains("restricted method"));
        assert!(!out.contains("rubygrapefruit"));
        assert!(!out.contains("enable-native-access"));
        assert!(!out.contains("Restricted methods will be blocked"));
        assert!(out.contains("BUILD SUCCESSFUL"));
    }

    #[test]
    fn test_filter_gradle_build_empty_falls_back_to_ok() {
        let input = "Starting a Gradle Daemon\nConfigure project :app\n";
        assert_eq!(filter_gradle_build(input), "gradle build: ok");
    }

    #[test]
    fn test_filter_gradle_test_preserves_deprecation_and_compile_errors() {
        let input = "\
Deprecated Gradle features were used in this build, making it incompatible with Gradle 9.0
/src/main/java/Foo.java:42: error: cannot find symbol
BUILD FAILED in 2s
";
        let out = filter_gradle_test(input);
        assert!(out.contains("Deprecated Gradle features were used"));
        assert!(out.contains("Foo.java:42"));
        assert!(out.contains("BUILD FAILED"));
        // Keeps Java compile-error lines by virtue of not matching any strip rule
        assert!(RE_JAVA_COMPILE_ERROR.is_match("/src/main/java/Foo.java:42: error: cannot find symbol"));
    }

    #[test]
    fn test_gradle_command_as_task_and_label() {
        assert_eq!(GradleCommand::Build.as_task(), "build");
        assert_eq!(GradleCommand::BootRun.as_task(), "bootRun");
        assert_eq!(GradleCommand::Test.label(), "gradle test");
        assert_eq!(GradleCommand::Clean.tee_label(), "gradle_clean");
    }
}
