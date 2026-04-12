//! Filters Maven (`mvn`) command output — strips scanning/download/progress
//! noise while preserving errors, warnings, test results, and final BUILD
//! SUCCESS/FAILURE banner.

use crate::core::runner;
use crate::core::utils::{exit_code_from_output, resolved_command, strip_ansi, truncate};
use anyhow::{Context, Result};
use regex::Regex;
use std::ffi::OsString;

/// Native Maven subcommands that rtk filters directly.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum MvnCommand {
    Build,
    Package,
    Clean,
    Install,
    Test,
    Verify,
}

impl MvnCommand {
    fn goal(self) -> &'static str {
        match self {
            // `mvn build` is a friendly alias for `compile` — Maven itself does
            // not have a `build` goal, so map to `compile`.
            MvnCommand::Build => "compile",
            MvnCommand::Package => "package",
            MvnCommand::Clean => "clean",
            MvnCommand::Install => "install",
            MvnCommand::Test => "test",
            MvnCommand::Verify => "verify",
        }
    }

    fn label(self) -> &'static str {
        match self {
            MvnCommand::Build => "mvn compile",
            MvnCommand::Package => "mvn package",
            MvnCommand::Clean => "mvn clean",
            MvnCommand::Install => "mvn install",
            MvnCommand::Test => "mvn test",
            MvnCommand::Verify => "mvn verify",
        }
    }

    fn tee_label(self) -> &'static str {
        match self {
            MvnCommand::Build => "mvn_build",
            MvnCommand::Package => "mvn_package",
            MvnCommand::Clean => "mvn_clean",
            MvnCommand::Install => "mvn_install",
            MvnCommand::Test => "mvn_test",
            MvnCommand::Verify => "mvn_verify",
        }
    }
}

/// Execute a known mvn goal with compact filtering.
pub fn run(cmd: MvnCommand, args: &[String], verbose: u8) -> Result<i32> {
    let mut command = resolved_command("mvn");
    command.arg(cmd.goal());
    for arg in args {
        command.arg(arg);
    }

    if verbose > 0 {
        eprintln!("Running: {} {}", cmd.label(), args.join(" "));
    }

    let label = cmd.label();
    let tee = cmd.tee_label();
    let filter = move |raw: &str| filter_for(cmd, &strip_ansi(raw));

    runner::run_filtered(
        command,
        label,
        &args.join(" "),
        filter,
        runner::RunOptions::with_tee(tee),
    )
}

/// Passthrough for any `mvn` subcommand rtk doesn't specialise. We still apply
/// the generic build filter since the noise patterns (Downloading, Progress,
/// reactor headers, JDK warnings) are consistent across goals.
pub fn run_passthrough(args: &[OsString], verbose: u8) -> Result<i32> {
    if args.is_empty() {
        anyhow::bail!("mvn: no goal specified");
    }

    let mut command = resolved_command("mvn");
    for arg in args {
        command.arg(arg);
    }

    if verbose > 0 {
        let joined = args
            .iter()
            .map(|a| a.to_string_lossy().into_owned())
            .collect::<Vec<_>>()
            .join(" ");
        eprintln!("Running: mvn {}", joined);
    }

    let output = command.output().context("Failed to run mvn")?;
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    let raw = format!("{}\n{}", stdout, stderr);

    let filtered = filter_mvn_build(&strip_ansi(&raw));
    println!("{}", filtered);

    Ok(exit_code_from_output(&output, "mvn"))
}

fn filter_for(cmd: MvnCommand, raw: &str) -> String {
    match cmd {
        MvnCommand::Test | MvnCommand::Verify => filter_mvn_test(raw),
        _ => filter_mvn_build(raw),
    }
}

/// Compile/package/install/clean filter — strip reactor/download/progress
/// noise, keep errors, warnings, and BUILD banner.
pub fn filter_mvn_build(output: &str) -> String {
    let mut kept: Vec<String> = Vec::new();

    for line in output.lines() {
        if is_mvn_noise(line) {
            continue;
        }
        if line.trim().is_empty() {
            continue;
        }
        kept.push(truncate(line.trim_end(), 240).to_string());
    }

    if kept.is_empty() {
        return "mvn: ok".to_string();
    }

    kept.join("\n")
}

/// Test/verify filter — like the build filter but also preserves test summary
/// lines (`Tests run:`) and surefire-style failure breadcrumbs.
pub fn filter_mvn_test(output: &str) -> String {
    let mut kept: Vec<String> = Vec::new();

    for line in output.lines() {
        if is_mvn_noise(line) {
            continue;
        }
        if line.trim().is_empty() {
            continue;
        }
        kept.push(truncate(line.trim_end(), 240).to_string());
    }

    if kept.is_empty() {
        return "mvn: ok".to_string();
    }

    kept.join("\n")
}

/// Patterns stripped from Maven output. Conservative — we never drop lines
/// starting with `[ERROR]`, `[WARNING]`, `Tests run:`, or `BUILD ...`.
fn is_mvn_noise(line: &str) -> bool {
    lazy_static::lazy_static! {
        static ref PATTERNS: Vec<Regex> = {
            let raw = [
                // Initial scan / reactor announcements
                r"^Scanning for projects",
                r"^\[INFO\] Scanning for projects",
                r"^\[INFO\] Reactor",
                r"^\[INFO\] -+$",
                r"^\[INFO\] =+$",
                r"^\[INFO\] $",
                r"^\[INFO\]\s*$",
                r"^\[INFO\] ---",
                r"^\[INFO\] Building\s",
                // Dependency I/O
                r"^Downloading from\s",
                r"^Downloaded from\s",
                r"^\[INFO\] Downloading\s",
                r"^\[INFO\] Downloaded\s",
                r"^Downloading:",
                r"^Downloaded:",
                r"^Progress \(\d+\)",
                r"^Progress ",
                // jansi / SLF4J / JDK restricted-method warnings
                r"^WARNING: A restricted method",
                r"^WARNING: java\.lang\.System::load",
                r"^WARNING: Use --enable-native-access",
                r"^WARNING: Restricted methods will be blocked",
            ];
            raw.iter()
                .map(|p| Regex::new(p).expect("invariant: static mvn pattern compiles"))
                .collect()
        };

        static ref KEEP_ERROR: Regex = Regex::new(r"^\[ERROR\]").unwrap();
        static ref KEEP_WARNING: Regex = Regex::new(r"^\[WARNING\]").unwrap();
        static ref KEEP_TESTS: Regex = Regex::new(r"^Tests run:").unwrap();
        static ref KEEP_BUILD: Regex = Regex::new(r"^(?:\[INFO\] )?BUILD (?:SUCCESS|FAILURE)").unwrap();
    }

    // Never strip these — the preserve list wins over the strip list.
    let trimmed = line.trim_start();
    if KEEP_ERROR.is_match(trimmed)
        || KEEP_WARNING.is_match(trimmed)
        || KEEP_TESTS.is_match(trimmed)
        || KEEP_BUILD.is_match(trimmed)
    {
        return false;
    }

    PATTERNS.iter().any(|re| re.is_match(line))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_filter_mvn_build_strips_noise_keeps_errors_and_banner() {
        let output = "\
Scanning for projects...
[INFO] ----------------------< com.example:app >----------------------
[INFO] Building app 1.0-SNAPSHOT
[INFO] Reactor Build Order:
Downloading from central: https://repo.maven.apache.org/foo.jar
Downloaded from central: https://repo.maven.apache.org/foo.jar (12 kB at 4 kB/s)
Progress (1): 4/12 kB
[INFO]
[INFO] --- maven-compiler-plugin:3.11.0:compile ---
[ERROR] /src/main/java/Main.java:[10,5] cannot find symbol
  symbol: method foo()
[INFO] BUILD FAILURE
[INFO] Total time: 2.543 s
";
        let filtered = filter_mvn_build(output);
        assert!(
            filtered.contains("[ERROR] /src/main/java/Main.java:[10,5] cannot find symbol"),
            "expected ERROR preserved, got:\n{}",
            filtered
        );
        assert!(filtered.contains("BUILD FAILURE"), "expected BUILD banner");
        assert!(!filtered.contains("Scanning for projects"));
        assert!(!filtered.contains("Downloading from"));
        assert!(!filtered.contains("Downloaded from"));
        assert!(!filtered.contains("Progress ("));
        assert!(!filtered.contains("Reactor Build Order"));
    }

    #[test]
    fn test_filter_mvn_build_strips_jdk_restricted_warnings() {
        let output = "\
WARNING: A restricted method in java.lang.System has been called
WARNING: java.lang.System::load has been called by org.fusesource.jansi
WARNING: Use --enable-native-access=ALL-UNNAMED to avoid
WARNING: Restricted methods will be blocked in a future release
[WARNING] This is a real compiler warning
[INFO] BUILD SUCCESS
";
        let filtered = filter_mvn_build(output);
        assert!(
            filtered.contains("[WARNING] This is a real compiler warning"),
            "compiler warnings must survive, got:\n{}",
            filtered
        );
        assert!(filtered.contains("BUILD SUCCESS"));
        assert!(!filtered.contains("restricted method in java.lang.System"));
        assert!(!filtered.contains("java.lang.System::load"));
        assert!(!filtered.contains("--enable-native-access"));
        assert!(!filtered.contains("Restricted methods will be blocked"));
    }

    #[test]
    fn test_filter_mvn_build_empty_returns_ok() {
        assert_eq!(filter_mvn_build(""), "mvn: ok");
        assert_eq!(
            filter_mvn_build("[INFO] \n[INFO] Reactor Build Order:\n"),
            "mvn: ok"
        );
    }

    #[test]
    fn test_filter_mvn_test_preserves_tests_run_summary() {
        let output = "\
[INFO] Scanning for projects...
Downloading from central: foo.jar
[INFO] -------------------------------------------------------
[INFO] T E S T S
[INFO] -------------------------------------------------------
Tests run: 42, Failures: 1, Errors: 0, Skipped: 0
[ERROR] Failures:
[ERROR]   com.example.FooTest.shouldWork:15 expected:<true> but was:<false>
[INFO] BUILD FAILURE
";
        let filtered = filter_mvn_test(output);
        assert!(
            filtered.contains("Tests run: 42, Failures: 1"),
            "test summary must survive, got:\n{}",
            filtered
        );
        assert!(filtered.contains("shouldWork:15 expected:<true>"));
        assert!(filtered.contains("BUILD FAILURE"));
        assert!(!filtered.contains("Downloading from"));
        assert!(!filtered.contains("Scanning for projects"));
    }

    #[test]
    fn test_mvn_command_goal_maps_build_to_compile() {
        assert_eq!(MvnCommand::Build.goal(), "compile");
        assert_eq!(MvnCommand::Package.goal(), "package");
        assert_eq!(MvnCommand::Clean.goal(), "clean");
        assert_eq!(MvnCommand::Install.goal(), "install");
        assert_eq!(MvnCommand::Test.goal(), "test");
        assert_eq!(MvnCommand::Verify.goal(), "verify");
    }
}
