//! Thin test helpers that *approximate* filter logic from src/cmds/jvm/ for use
//! in integration tests. They cannot share code with the binary crate (its
//! modules are inaccessible from `tests/` in a bin-only crate), so the patterns
//! here are duplicated by hand.
//!
//! **Caveat:** this file can drift from production. The integration tests are
//! a smoke check on representative compression, NOT a byte-for-byte regression
//! suite. Treat them as: "the production filter must compress fixtures at least
//! as well as this minimal subset". For exact filter behavior, see the inline
//! TOML tests in `src/filters/*.toml` and the per-module unit tests in
//! `src/cmds/jvm/*.rs`.

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
    if out.trim().is_empty() {
        "gradle test: ok".to_string()
    } else {
        out
    }
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
    if out.trim().is_empty() {
        "ant: ok".to_string()
    } else {
        out
    }
}
