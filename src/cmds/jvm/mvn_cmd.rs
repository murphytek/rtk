//! Filters Maven (`mvn`) command output — strips scanning/download/progress
//! noise while preserving errors, warnings, test results, and final BUILD
//! SUCCESS/FAILURE banner.

use crate::cmds::jvm::line_dedup::dedupe_repeated_lines;
use crate::cmds::jvm::stack_dedup::dedupe_stack_traces;
use crate::cmds::jvm::stack_trim::trim_stack_noise;
use crate::core::runner;
use crate::core::utils::{
    exit_code_from_output, resolved_build_command, strip_ansi, truncate,
};
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

/// Probe the version of the Maven binary the given command is configured to invoke.
/// Returns `Some((major, minor, patch))` when `<bin> --version` succeeds and parses,
/// `None` otherwise. Results are cached per-binary so the probe runs at most once
/// per process per resolved binary.
fn probe_mvn_version(command: &std::process::Command) -> Option<(u32, u32, u32)> {
    use std::collections::HashMap;
    use std::sync::Mutex;

    lazy_static::lazy_static! {
        static ref CACHE: Mutex<HashMap<std::ffi::OsString, Option<(u32, u32, u32)>>> =
            Mutex::new(HashMap::new());
    }

    let bin = command.get_program().to_os_string();

    if let Ok(cache) = CACHE.lock() {
        if let Some(cached) = cache.get(&bin) {
            return *cached;
        }
    }

    let parsed = std::process::Command::new(&bin)
        .arg("--version")
        .output()
        .ok()
        .and_then(|o| {
            let text = String::from_utf8_lossy(&o.stdout).into_owned();
            parse_mvn_version_string(&text)
        });

    if let Ok(mut cache) = CACHE.lock() {
        cache.insert(bin, parsed);
    }
    parsed
}

/// Parse "Apache Maven 3.6.1 (...)" → (3, 6, 1). Tolerant of leading whitespace
/// and missing patch component (assumes 0).
fn parse_mvn_version_string(text: &str) -> Option<(u32, u32, u32)> {
    let line = text.lines().find(|l| l.trim_start().starts_with("Apache Maven "))?;
    let after_label = line.trim_start().trim_start_matches("Apache Maven ");
    let version_token = after_label.split_whitespace().next()?;
    let mut parts = version_token.split('.');
    let major: u32 = parts.next()?.parse().ok()?;
    let minor: u32 = parts.next()?.parse().ok()?;
    let patch: u32 = parts.next().and_then(|p| p.parse().ok()).unwrap_or(0);
    Some((major, minor, patch))
}

/// `--no-transfer-progress` was added in Maven 3.6.1. Older releases reject it
/// with "Unrecognized option" and exit immediately, breaking the whole build.
fn supports_no_transfer_progress(command: &std::process::Command) -> bool {
    match probe_mvn_version(command) {
        Some(v) => v >= (3, 6, 1),
        // Fail-CLOSED: if we can't determine the version, don't inject — better
        // to lose a small win than to break someone's build.
        None => false,
    }
}

/// Inject `--no-transfer-progress` (kills download chatter, Maven 3.6.1+ only)
/// and `-B` (batch mode, no ANSI colors / interactive prompts; supported by
/// every Maven we care about). Skips injection when the user already passed
/// either flag or its short equivalent.
fn inject_quiet_flags(command: &mut std::process::Command, args: &[String]) {
    let already_has = |flag: &str, short: Option<&str>| {
        args.iter().any(|a| {
            a == flag
                || short.map(|s| a == s).unwrap_or(false)
                || a.starts_with(&format!("{}=", flag))
        })
    };
    if !already_has("--no-transfer-progress", Some("-ntp")) && supports_no_transfer_progress(command) {
        command.arg("--no-transfer-progress");
    }
    if !already_has("--batch-mode", Some("-B")) {
        command.arg("-B");
    }
}

/// Same as [`inject_quiet_flags`] but for the OsString-based passthrough path.
fn inject_quiet_flags_os(command: &mut std::process::Command, args: &[OsString]) {
    let already_has = |flag: &str, short: Option<&str>| {
        args.iter().any(|a| {
            a == flag
                || short.map(|s| a == s).unwrap_or(false)
                || a.to_string_lossy().starts_with(&format!("{}=", flag))
        })
    };
    if !already_has("--no-transfer-progress", Some("-ntp")) && supports_no_transfer_progress(command) {
        command.arg("--no-transfer-progress");
    }
    if !already_has("--batch-mode", Some("-B")) {
        command.arg("-B");
    }
}

/// Execute a known mvn goal with compact filtering.
pub fn run(cmd: MvnCommand, args: &[String], verbose: u8) -> Result<i32> {
    let mut command = resolved_build_command("mvn");
    inject_quiet_flags(&mut command, args);
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

    let mut command = resolved_build_command("mvn");
    inject_quiet_flags_os(&mut command, args);
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

    let filtered = filter_mvn_build(&collapse_pgp_block(&strip_ansi(&raw)));
    println!("{}", filtered);

    Ok(exit_code_from_output(&output, "mvn"))
}

fn filter_for(cmd: MvnCommand, raw: &str) -> String {
    let collapsed = collapse_pgp_block(raw);
    match cmd {
        MvnCommand::Test | MvnCommand::Verify => filter_mvn_test(&collapsed),
        _ => filter_mvn_build(&collapsed),
    }
}

/// Collapse pgpverify-maven-plugin's per-artifact chatter into a single summary
/// line. Each verified artifact emits two lines (`artifact ... PGP Signature
/// OK` + an indented `KeyId:` continuation), which dwarfs the actual build
/// output on dependency-heavy projects. We fold any run of OK results into
/// `[INFO] pgpverify: N artifacts verified`, while leaving FAILED/INVALID/
/// MISSING lines intact for diagnosis.
pub fn collapse_pgp_block(input: &str) -> String {
    lazy_static::lazy_static! {
        static ref PGP_LINE: Regex =
            Regex::new(r"^\[INFO\] artifact .+ PGP Signature (OK|FAILED|INVALID|MISSING)\b")
                .expect("invariant: pgp line pattern compiles");
        static ref PGP_KEYID: Regex =
            Regex::new(r"^\[INFO\]\s+KeyId:\s").expect("invariant: pgp keyid pattern compiles");
    }

    let lines: Vec<&str> = input.lines().collect();
    let mut out: Vec<String> = Vec::with_capacity(lines.len());
    let mut i = 0;
    while i < lines.len() {
        let line = lines[i];
        if PGP_LINE.is_match(line) {
            // We're entering a PGP block. Scan forward collecting consecutive
            // PGP lines (plus their optional KeyId continuation).
            let mut ok_count: usize = 0;
            let mut non_ok: Vec<String> = Vec::new();
            let mut j = i;
            while j < lines.len() {
                let cur = lines[j];
                let Some(cur_caps) = PGP_LINE.captures(cur) else {
                    break;
                };
                let status = cur_caps.get(1).map(|m| m.as_str()).unwrap_or("");
                let is_ok = status == "OK";
                if is_ok {
                    ok_count += 1;
                } else {
                    non_ok.push(cur.to_string());
                }
                j += 1;
                // Absorb an optional KeyId continuation line.
                if j < lines.len() && PGP_KEYID.is_match(lines[j]) {
                    if !is_ok {
                        non_ok.push(lines[j].to_string());
                    }
                    j += 1;
                }
            }

            if ok_count > 0 {
                let noun = if ok_count == 1 { "artifact" } else { "artifacts" };
                out.push(format!("[INFO] pgpverify: {} {} verified", ok_count, noun));
            }
            for failure in non_ok {
                out.push(failure);
            }
            i = j;
            continue;
        }
        out.push(line.to_string());
        i += 1;
    }

    out.join("\n")
}

/// Compile/package/install/clean filter — strip reactor/download/progress
/// noise, keep errors, warnings, and BUILD banner.
pub fn filter_mvn_build(output: &str) -> String {
    // Collapse repeated stack traces first, trim intra-trace noise, then
    // collapse repeated single lines (e.g. 15x "WARNING: Unsupported Kotlin
    // plugin version") before per-line stripping.
    let deduped = dedupe_repeated_lines(&trim_stack_noise(&dedupe_stack_traces(output)));
    let mut kept: Vec<String> = Vec::new();

    for line in deduped.lines() {
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
    // JUnit / surefire frequently emit identical runner stack traces once per
    // failed test; dedupe before line-level filtering.
    let deduped = dedupe_repeated_lines(&trim_stack_noise(&dedupe_stack_traces(output)));
    let mut kept: Vec<String> = Vec::new();

    for line in deduped.lines() {
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
                // JVM 25 sun.misc.Unsafe deprecation footer block (4 lines)
                r"^WARNING: A terminally deprecated method in sun\.misc\.Unsafe",
                r"^WARNING: sun\.misc\.Unsafe::objectFieldOffset",
                r"^WARNING: This is a critical method",
                // pgpverify-maven-plugin config-block chatter
                r"^\[INFO\] Key server\(s\)",
                r"^\[INFO\] Create cache directory for PGP keys:",
                r"^\[INFO\] Resolved \d+ artifact\(s\)",
                r"^\[INFO\] Artifacts were already validated",
                // Corpus-discovered patterns (resources, checkstyle, OpenAPI gen)
                r"^\[INFO\] writing file ",
                r"^\[INFO\] Preparing remote bundle ",
                r"^\[INFO\] Copying \d+ resource",
                r"^\[INFO\] You have 0 Checkstyle violations\.",
                r"^\[INFO\] Starting audit\.\.\.",
                r"^\[INFO\] Audit done\.",
                r"^\[INFO\] Using 'UTF-8' encoding to copy filtered resources\.",
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
    fn test_collapse_pgp_block_all_ok() {
        let input = "\
[INFO] --- pgpverify-maven-plugin:1.18.4:check (default) @ htmlunit ---
[INFO] artifact org.springframework:spring-core:jar:6.1.0:compile PGP Signature OK
[INFO]        KeyId: 0xB1C73B62B6E2C8A1 UserIds: [Spring IO <ci@spring.io>]
[INFO] artifact org.springframework:spring-context:jar:6.1.0:compile PGP Signature OK
[INFO]        KeyId: 0xB1C73B62B6E2C8A1 UserIds: [Spring IO <ci@spring.io>]
[INFO] artifact net.sourceforge.htmlunit:foo:jar:1.0:compile PGP Signature OK
[INFO]        KeyId: 0xDEADBEEF UserIds: [Test]
[INFO] BUILD SUCCESS";
        let collapsed = collapse_pgp_block(input);
        assert!(
            collapsed.contains("[INFO] pgpverify: 3 artifacts verified"),
            "expected summary, got:\n{}",
            collapsed
        );
        assert!(!collapsed.contains("PGP Signature OK"));
        assert!(!collapsed.contains("KeyId:"));
        assert!(collapsed.contains("[INFO] --- pgpverify-maven-plugin"));
        assert!(collapsed.contains("[INFO] BUILD SUCCESS"));
    }

    #[test]
    fn test_collapse_pgp_block_mixed_ok_and_failed() {
        let input = "\
[INFO] artifact org.springframework:spring-core:jar:6.1.0:compile PGP Signature OK
[INFO]        KeyId: 0xAAA UserIds: [A]
[INFO] artifact com.shady:bad:jar:1.0:compile PGP Signature FAILED
[INFO]        KeyId: 0xBAD UserIds: [Evil]
[INFO] artifact com.shady:worse:jar:1.0:compile PGP Signature MISSING
[INFO] artifact org.ok:good:jar:1.0:compile PGP Signature OK
[INFO]        KeyId: 0xCCC UserIds: [C]
[INFO] BUILD FAILURE";
        let collapsed = collapse_pgp_block(input);
        assert!(
            collapsed.contains("[INFO] pgpverify: 2 artifacts verified"),
            "expected summary for 2 OK artifacts, got:\n{}",
            collapsed
        );
        assert!(
            collapsed.contains("com.shady:bad:jar:1.0:compile PGP Signature FAILED"),
            "FAILED line must survive, got:\n{}",
            collapsed
        );
        assert!(
            collapsed.contains("com.shady:worse:jar:1.0:compile PGP Signature MISSING"),
            "MISSING line must survive, got:\n{}",
            collapsed
        );
        assert!(
            collapsed.contains("KeyId: 0xBAD"),
            "failure KeyId context should be kept, got:\n{}",
            collapsed
        );
        assert!(
            !collapsed.contains("PGP Signature OK"),
            "OK lines should be collapsed away, got:\n{}",
            collapsed
        );
        assert!(!collapsed.contains("KeyId: 0xAAA"));
        assert!(!collapsed.contains("KeyId: 0xCCC"));
        assert!(collapsed.contains("[INFO] BUILD FAILURE"));
    }

    #[test]
    fn test_collapse_pgp_block_no_pgp_passthrough() {
        let input = "\
[INFO] Scanning for projects...
[INFO] Building app 1.0
[INFO] --- maven-compiler-plugin:3.11.0:compile ---
[ERROR] compile failed
[INFO] BUILD FAILURE";
        let collapsed = collapse_pgp_block(input);
        assert_eq!(collapsed, input);
    }

    #[test]
    fn test_collapse_pgp_block_singular_artifact_noun() {
        let input = "\
[INFO] artifact foo:bar:jar:1.0:compile PGP Signature OK
[INFO]        KeyId: 0x1 UserIds: [x]";
        let collapsed = collapse_pgp_block(input);
        assert_eq!(collapsed, "[INFO] pgpverify: 1 artifact verified");
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
