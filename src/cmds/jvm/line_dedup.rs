//! Line-level repeated-line deduplication for JVM filter pipelines.
//!
//! Distinct from stack-trace dedup: this collapses adjacent runs of identical
//! lines (matched after stripping leading whitespace). Runs of N >= 3 are
//! replaced with the first occurrence plus `(... repeated N-1 more times ...)`.
//! Runs of N < 3 are passed through unchanged.

/// Collapse adjacent runs of identical lines (after leading-whitespace trim).
///
/// For a run of N >= 3 identical lines, emits:
///   `<original line>\n(... repeated N-1 more times ...)`
/// For N < 3, passes through unchanged.
pub fn dedupe_repeated_lines(input: &str) -> String {
    if input.is_empty() {
        return String::new();
    }

    let lines: Vec<&str> = input.split('\n').collect();
    let has_trailing_newline = matches!(lines.last(), Some(&""));
    let slice_end = if has_trailing_newline {
        lines.len() - 1
    } else {
        lines.len()
    };

    let mut out: Vec<String> = Vec::with_capacity(slice_end);
    let mut i = 0;

    while i < slice_end {
        let line = lines[i];
        let key = line.trim_start();

        // Count how many consecutive lines share the same trimmed content.
        let mut run = 1;
        while i + run < slice_end && lines[i + run].trim_start() == key {
            run += 1;
        }

        if run >= 3 {
            out.push(line.to_string());
            out.push(format!("(... repeated {} more times ...)", run - 1));
        } else {
            for k in 0..run {
                out.push(lines[i + k].to_string());
            }
        }

        i += run;
    }

    let mut joined = out.join("\n");
    if has_trailing_newline {
        joined.push('\n');
    }
    joined
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn three_adjacent_identical_lines_collapse() {
        let input = "WARNING: Unsupported Kotlin plugin version\n\
                     WARNING: Unsupported Kotlin plugin version\n\
                     WARNING: Unsupported Kotlin plugin version\n";
        let out = dedupe_repeated_lines(input);
        assert!(
            out.contains("WARNING: Unsupported Kotlin plugin version"),
            "first line must survive: {}",
            out
        );
        assert!(
            out.contains("(... repeated 2 more times ...)"),
            "expected repeat marker, got:\n{}",
            out
        );
        // Original line should appear exactly once.
        assert_eq!(
            out.matches("WARNING: Unsupported Kotlin plugin version").count(),
            1
        );
    }

    #[test]
    fn two_identical_lines_pass_through_unchanged() {
        let input = "foo\nfoo\nbar\n";
        let out = dedupe_repeated_lines(input);
        assert_eq!(out, input);
    }

    #[test]
    fn five_then_different_then_five_two_collapses() {
        let input = "> Configure project :app\n\
                     > Configure project :app\n\
                     > Configure project :app\n\
                     > Configure project :app\n\
                     > Configure project :app\n\
                     BUILD SUCCESSFUL\n\
                     > Configure project :app\n\
                     > Configure project :app\n\
                     > Configure project :app\n\
                     > Configure project :app\n\
                     > Configure project :app\n";
        let out = dedupe_repeated_lines(input);
        assert_eq!(
            out.matches("(... repeated 4 more times ...)").count(),
            2,
            "expected 2 collapse markers, got:\n{}",
            out
        );
        assert!(out.contains("BUILD SUCCESSFUL"));
    }

    #[test]
    fn plain_text_passes_through_unchanged() {
        let input = "[INFO] Building app 1.0\n\
                     [INFO] Compiling sources\n\
                     BUILD SUCCESS\n";
        let out = dedupe_repeated_lines(input);
        assert_eq!(out, input);
    }

    #[test]
    fn leading_whitespace_variants_collapse_together() {
        // Lines with different leading whitespace but same trimmed content.
        let input = "  foo\n  foo\n  foo\n";
        let out = dedupe_repeated_lines(input);
        assert!(
            out.contains("(... repeated 2 more times ...)"),
            "whitespace-trimmed duplicates should collapse: {}",
            out
        );
    }

    #[test]
    fn trailing_newline_preserved() {
        let input = "a\nb\nc\n";
        let out = dedupe_repeated_lines(input);
        assert!(out.ends_with('\n'));
    }

    #[test]
    fn no_trailing_newline_preserved() {
        let input = "a\nb\nc";
        let out = dedupe_repeated_lines(input);
        assert!(!out.ends_with('\n'));
    }
}
