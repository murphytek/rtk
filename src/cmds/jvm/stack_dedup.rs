//! Stack-trace deduplication for JVM filter pipelines.
//!
//! Many JVM tools (SpotBugs, Surefire, Failsafe, JUnit runners) repeat the
//! same multi-line stack trace dozens or hundreds of times per run. One real
//! benchmark saw a single 14-line SpotBugs trace repeated 340 times (~4800
//! lines of pure noise). This module detects contiguous "stack frame blocks"
//! — an exception header line followed by one or more `    at ...` frames —
//! and collapses consecutive identical blocks into the first occurrence plus
//! a single `(... repeated N more times ...)` marker.
//!
//! The heuristic is intentionally conservative: when in doubt, a block is
//! left untouched. A block's identity is its exact textual content (string
//! equality), so whitespace-identical traces collapse and anything else is
//! preserved.
//!
//! Exposed as `dedupe_stack_traces(&str) -> String` and wired into
//! `filter_mvn_*` / `filter_gradle_*` as a pre-processing pass.
//!
//! Detection rules:
//! - The first line of a block matches either the exception header pattern
//!   (`<package>.<Class>Exception|Error: ...`) or begins with `Caused by:`.
//! - Subsequent lines start with `\s+at\s` (standard Java stack frame).
//! - An optional `... N more` continuation line is absorbed into the block.
//! - Any other line terminates the block.

use lazy_static::lazy_static;
use regex::Regex;

lazy_static! {
    /// Matches an exception / error header, e.g.
    /// `java.lang.RuntimeException: Boom` or `com.foo.MyError`.
    /// We require at least one dotted segment and an `Exception`/`Error`
    /// suffix on the final class name to avoid false positives on ordinary
    /// prose lines.
    static ref EXCEPTION_HEADER: Regex = Regex::new(
        r"^(?:[A-Za-z_][\w$]*\.)+[A-Za-z_][\w$]*(?:Exception|Error|Throwable)(?::.*)?$"
    )
    .expect("invariant: exception header pattern compiles");

    /// Matches a `Caused by:` continuation, which also begins a fresh block.
    static ref CAUSED_BY: Regex =
        Regex::new(r"^Caused by:\s").expect("invariant: caused-by pattern compiles");

    /// Matches a standard Java stack frame continuation: leading whitespace +
    /// `at` + space + anything.
    static ref AT_FRAME: Regex =
        Regex::new(r"^\s+at\s").expect("invariant: at-frame pattern compiles");

    /// Matches the `... N more` truncation line surefire / JVM emits at the
    /// tail of a nested cause.
    static ref MORE_FRAMES: Regex =
        Regex::new(r"^\s+\.\.\.\s+\d+\s+more\s*$").expect("invariant: more-frames pattern compiles");
}

/// Returns true if `line` can start a stack-frame block.
fn is_block_header(line: &str) -> bool {
    EXCEPTION_HEADER.is_match(line.trim_start()) || CAUSED_BY.is_match(line.trim_start())
}

/// Returns true if `line` is a valid continuation frame within a block.
fn is_block_continuation(line: &str) -> bool {
    AT_FRAME.is_match(line) || MORE_FRAMES.is_match(line) || CAUSED_BY.is_match(line.trim_start())
}

/// Collapse consecutive identical stack-frame blocks.
///
/// Preserves all non-stack content verbatim, including blank lines and
/// trailing newline semantics — output lines are joined with `\n` and a
/// trailing newline is appended iff the input ends with one.
pub fn dedupe_stack_traces(input: &str) -> String {
    if input.is_empty() {
        return String::new();
    }

    let lines: Vec<&str> = input.split('\n').collect();
    // If the input ends with '\n' the final split entry is an empty string;
    // detect that and reattach a trailing newline at the end.
    let has_trailing_newline = matches!(lines.last(), Some(&""));
    let slice_end = if has_trailing_newline {
        lines.len() - 1
    } else {
        lines.len()
    };

    let mut out: Vec<String> = Vec::with_capacity(slice_end);
    let mut i = 0;
    // Track the most recently emitted block so the NEXT matching block can
    // be suppressed. Cleared whenever a non-block line is emitted.
    let mut last_block: Option<String> = None;
    let mut last_block_repeat: usize = 0;

    while i < slice_end {
        let line = lines[i];
        if is_block_header(line) {
            // Consume the block: header + contiguous continuation frames.
            let start = i;
            i += 1;
            while i < slice_end && is_block_continuation(lines[i]) {
                // A `Caused by:` starts a new logical section but we treat
                // it as part of the same trace for dedup purposes — it
                // gives identical causal chains a single identity.
                i += 1;
            }
            let block_slice = &lines[start..i];
            let block_text = block_slice.join("\n");

            match &last_block {
                Some(prev) if prev == &block_text => {
                    last_block_repeat += 1;
                }
                _ => {
                    flush_repeat(&mut out, last_block_repeat);
                    out.push(block_text.clone());
                    last_block = Some(block_text);
                    last_block_repeat = 0;
                }
            }
            continue;
        }

        // Non-block line. A blank line is treated as a transparent separator
        // between identical blocks — i.e. `block\n\nblock` still collapses.
        // Any non-blank, non-block line flushes the pending repeat counter
        // and resets the dedup state.
        if line.trim().is_empty() {
            // Peek ahead past any run of blank lines; if the very next
            // non-blank line opens a block identical to `last_block`,
            // swallow the blank lines (they belong to the repeat cluster).
            if let Some(prev) = &last_block {
                let mut j = i;
                while j < slice_end && lines[j].trim().is_empty() {
                    j += 1;
                }
                if j < slice_end && is_block_header(lines[j]) {
                    // Speculatively extract the upcoming block.
                    let bstart = j;
                    j += 1;
                    while j < slice_end && is_block_continuation(lines[j]) {
                        j += 1;
                    }
                    let next_block = lines[bstart..j].join("\n");
                    if &next_block == prev {
                        // Swallow the blank run AND the duplicate block.
                        last_block_repeat += 1;
                        i = j;
                        continue;
                    }
                }
            }
            // Otherwise, blank line flushes the counter and passes through.
            flush_repeat(&mut out, last_block_repeat);
            last_block_repeat = 0;
            last_block = None;
            out.push(line.to_string());
            i += 1;
            continue;
        }

        flush_repeat(&mut out, last_block_repeat);
        last_block_repeat = 0;
        last_block = None;
        out.push(line.to_string());
        i += 1;
    }

    flush_repeat(&mut out, last_block_repeat);

    let mut joined = out.join("\n");
    if has_trailing_newline {
        joined.push('\n');
    }
    joined
}

fn flush_repeat(out: &mut Vec<String>, n: usize) {
    if n > 0 {
        out.push(format!("(... repeated {} more times ...)", n));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const TRACE: &str = "\
java.lang.RuntimeException: Boom
    at com.foo.Bar.run(Bar.java:42)
    at com.foo.Bar.main(Bar.java:10)";

    #[test]
    fn three_identical_traces_collapse() {
        let input = format!("{}\n\n{}\n\n{}\n\nBUILD FAILED\n", TRACE, TRACE, TRACE);
        let out = dedupe_stack_traces(&input);
        assert!(
            out.contains("java.lang.RuntimeException: Boom"),
            "first trace must survive: {}",
            out
        );
        assert!(
            out.contains("(... repeated 2 more times ...)"),
            "expected repeat marker, got:\n{}",
            out
        );
        // The trace text should appear exactly once in the output.
        let occurrences = out.matches("at com.foo.Bar.run(Bar.java:42)").count();
        assert_eq!(occurrences, 1, "expected 1 frame occurrence, got:\n{}", out);
        assert!(out.contains("BUILD FAILED"));
    }

    #[test]
    fn two_different_traces_both_kept() {
        let a = "\
java.lang.RuntimeException: A
    at com.foo.A.run(A.java:1)";
        let b = "\
java.lang.IllegalStateException: B
    at com.foo.B.run(B.java:2)";
        let input = format!("{}\n\n{}\n", a, b);
        let out = dedupe_stack_traces(&input);
        assert!(out.contains("RuntimeException: A"));
        assert!(out.contains("IllegalStateException: B"));
        assert!(
            !out.contains("repeated"),
            "should not dedupe distinct traces: {}",
            out
        );
    }

    #[test]
    fn single_trace_unchanged() {
        let input = format!("{}\nBUILD FAILED\n", TRACE);
        let out = dedupe_stack_traces(&input);
        assert_eq!(out, input);
    }

    #[test]
    fn plain_text_unchanged() {
        let input =
            "[INFO] Building foo\n[INFO] compiling 42 sources\nBUILD SUCCESSFUL in 3s\n";
        let out = dedupe_stack_traces(input);
        assert_eq!(out, input);
    }

    #[test]
    fn non_adjacent_duplicates_are_kept() {
        // Unrelated text between duplicates breaks the run and both survive.
        let input = format!(
            "{}\n\n[INFO] other progress\n\n{}\n\nBUILD FAILED\n",
            TRACE, TRACE
        );
        let out = dedupe_stack_traces(&input);
        assert_eq!(
            out.matches("at com.foo.Bar.run(Bar.java:42)").count(),
            2,
            "traces separated by non-blank content must both survive:\n{}",
            out
        );
        assert!(!out.contains("repeated"));
    }

    #[test]
    fn caused_by_block_collapses() {
        let trace = "\
java.lang.RuntimeException: Wrap
    at com.foo.Bar.run(Bar.java:42)
Caused by: java.io.IOException: inner
    at com.foo.Bar.read(Bar.java:99)";
        let input = format!("{}\n\n{}\n", trace, trace);
        let out = dedupe_stack_traces(&input);
        assert!(out.contains("Caused by: java.io.IOException: inner"));
        assert!(
            out.contains("(... repeated 1 more times ...)"),
            "expected repeat marker, got:\n{}",
            out
        );
    }
}
