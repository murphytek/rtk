//! Thin test helpers that replicate filter logic from src/cmds/jvm/ for use
//! in integration tests. These are intentionally minimal — they use the same
//! regex patterns as the production code but don't depend on the binary crate's
//! internal module graph (which is inaccessible from tests/ in a bin-only crate).

use regex::Regex;

fn strip_lines(input: &str, patterns: &[Regex], keep: &[Regex]) -> String {
    let mut kept: Vec<&str> = Vec::new();
    for line in input.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        // Preserved lines always win.
        if keep.iter().any(|re| re.is_match(trimmed)) {
            kept.push(line);
            continue;
        }
        if patterns.iter().any(|re| re.is_match(line)) {
            continue;
        }
        kept.push(line);
    }
    kept.join("\n")
}

/// Minimal Maven build filter — same pattern set as `is_mvn_noise` in mvn_cmd.rs.
pub fn filter_mvn_build(output: &str) -> String {
    lazy_static::lazy_static! {
        static ref STRIP: Vec<Regex> = [
            r"^Scanning for projects",
            r"^\[INFO\] Scanning for projects",
            r"^\[INFO\] Reactor",
            r"^\[INFO\] -+$",
            r"^\[INFO\] =+$",
            r"^\[INFO\] $",
            r"^\[INFO\]\s*$",
            r"^\[INFO\] ---",
            r"^\[INFO\] Building\s",
            r"^Downloading from\s",
            r"^Downloaded from\s",
            r"^\[INFO\] Downloading\s",
            r"^\[INFO\] Downloaded\s",
            r"^Downloading:",
            r"^Downloaded:",
            r"^Progress \(\d+\)",
            r"^Progress ",
            r"^WARNING: A restricted method",
            r"^WARNING: java\.lang\.System::load",
            r"^WARNING: Use --enable-native-access",
            r"^WARNING: Restricted methods will be blocked",
            r"^WARNING: A terminally deprecated method in sun\.misc\.Unsafe",
            r"^WARNING: sun\.misc\.Unsafe::objectFieldOffset",
            r"^WARNING: This is a critical method",
            r"^SLF4J:",
            r"^\[INFO\] Key server\(s\)",
            r"^\[INFO\] Create cache directory for PGP keys:",
            r"^\[INFO\] Resolved \d+ artifact\(s\)",
            r"^\[INFO\] Artifacts were already validated",
            // Corpus-discovered patterns
            r"^\[INFO\] writing file ",
            r"^\[INFO\] Preparing remote bundle ",
            r"^\[INFO\] Copying \d+ resource",
            r"^\[INFO\] You have 0 Checkstyle violations\.",
            r"^\[INFO\] Starting audit\.\.\.",
            r"^\[INFO\] Audit done\.",
            r"^\[INFO\] Using 'UTF-8' encoding to copy filtered resources\.",
            r"^\[INFO\] skip non existing resourceDirectory",
            r"^\[INFO\] Finished at:",
            r"^\[INFO\] --< ",
            r"^\[INFO\]\s+from ",
            r"^\[INFO\]\s+T E S T S",
            r"^\[INFO\] Results:",
        ].iter().map(|p| Regex::new(p).unwrap()).collect();

        static ref KEEP: Vec<Regex> = [
            r"^\[ERROR\]",
            r"^\[WARNING\]",
            r"^Tests run:",
            r"^(?:\[INFO\] )?BUILD (?:SUCCESS|FAILURE)",
        ].iter().map(|p| Regex::new(p).unwrap()).collect();
    }
    let out = strip_lines(output, &STRIP, &KEEP);
    if out.trim().is_empty() { "mvn: ok".to_string() } else { out }
}

/// Maven test filter — same as build filter (surefire output is handled the same way).
pub fn filter_mvn_test(output: &str) -> String {
    filter_mvn_build(output)
}

/// Minimal Gradle test filter — strips daemon/progress/configure chatter,
/// keeps FAILED task lines and BUILD FAILED/SUCCESSFUL.
pub fn filter_gradle_test(output: &str) -> String {
    lazy_static::lazy_static! {
        static ref STRIP: Vec<Regex> = [
            r"^Starting a Gradle Daemon",
            r"^Configure project ",
            r"^<\d+/\d+ tasks>",
            r"^\d+ actionable tasks?",
            r"^> Task .+UP-TO-DATE$",
            r"^> Task .+SKIPPED$",
            r"^> Task .+FROM-CACHE$",
            r"^> Task .+NO-SOURCE$",
            r"^> Task :[^ ]+ $",
        ].iter().map(|p| Regex::new(p).unwrap()).collect();

        static ref KEEP: Vec<Regex> = [
            r"^FAILURE:",
            r"^BUILD FAILED",
            r"^BUILD SUCCESSFUL",
            r"^\* What went wrong:",
            r"^\* Try:",
            r"^> Task .+FAILED",
        ].iter().map(|p| Regex::new(p).unwrap()).collect();
    }

    // Collapse repeated identical lines (SpotBugs stack trace repeated N times)
    let deduped = dedupe_repeated_blocks(output);
    let out = strip_lines(&deduped, &STRIP, &KEEP);
    if out.trim().is_empty() { "gradle test: ok".to_string() } else { out }
}

/// Collapse consecutive identical blocks of lines that repeat ≥2 times,
/// keeping one copy of each block. Scans the whole input for any run of
/// repeating blocks (not just from position 0).
fn dedupe_repeated_blocks(input: &str) -> String {
    let lines: Vec<&str> = input.lines().collect();
    if lines.len() < 4 {
        return input.to_string();
    }

    let mut result: Vec<&str> = Vec::with_capacity(lines.len());
    let mut i = 0;

    while i < lines.len() {
        // Try block sizes from large to small to find the longest repeating block.
        let max_block = (lines.len() - i) / 2;
        let mut found = false;
        for block_size in (2..=max_block).rev() {
            let block = &lines[i..i + block_size];
            let mut j = i + block_size;
            let mut count = 1usize;
            while j + block_size <= lines.len() && lines[j..j + block_size] == *block {
                count += 1;
                j += block_size;
            }
            if count >= 2 {
                // Emit one copy of the block.
                result.extend_from_slice(block);
                i = j; // skip all repetitions
                found = true;
                break;
            }
        }
        if !found {
            result.push(lines[i]);
            i += 1;
        }
    }

    result.join("\n")
}

/// Minimal Ant build/compile filter — strips target headers, task chatter,
/// keeps errors and BUILD result.
pub fn filter_ant_build(output: &str) -> String {
    lazy_static::lazy_static! {
        static ref STRIP: Vec<Regex> = [
            r"^Buildfile:",
            r"^\w[\w-]*:$",          // bare target name like "compile:" or "clean:"
            r"^\s+\[echo\]",
            r"^\s+\[mkdir\]",
            r"^\s+\[delete\]",
            r"^\s+\[copy\]",
            r"^\s+\[javac\] Compiling \d+",
            r"^\s+\[javac\] Note:",
        ].iter().map(|p| Regex::new(p).unwrap()).collect();

        static ref KEEP: Vec<Regex> = [
            r"^BUILD (SUCCESSFUL|FAILED)",
            r"^Total time:",
            r"^\s+\[javac\].*error:",
            r"^\s+\[javac\].*\^",
            r"^\s+\[javac\]  ",
            r"^\s+\[javac\] \d+ error",
        ].iter().map(|p| Regex::new(p).unwrap()).collect();
    }
    let out = strip_lines(output, &STRIP, &KEEP);
    if out.trim().is_empty() { "ant: ok".to_string() } else { out }
}
