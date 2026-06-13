//! tree command - proxy to native tree with token-optimized output
//!
//! This module proxies to the native `tree` command and filters the output
//! to reduce token usage while preserving structure visibility.
//!
//! Token optimization: automatically excludes noise directories via -I pattern
//! unless -a flag is present (respecting user intent).

use super::constants::NOISE_DIRS;
use crate::core::runner::{self, RunOptions};
use crate::core::utils::{resolved_command, tool_exists};
use anyhow::Result;
use lazy_static::lazy_static;
use regex::Regex;

lazy_static! {
    /// Matches `tree`'s trailing summary line, e.g.
    ///   "5 directories, 23 files"
    ///   "1 directory, 0 files"
    ///   "3 directories, 5 files, 2 links"
    ///   "0 directories"
    /// Anchored to the whole (trimmed) line so it can never match a legitimate
    /// entry whose filename merely contains the words "director"/"file". This is
    /// the fix for the over-matching `contains("director") && contains("file")`
    /// heuristic that silently dropped entries like `director-files.txt`.
    static ref TREE_SUMMARY_RE: Regex = Regex::new(
        r"^\d+ director(?:y|ies)(?:, \d+ files?)?(?:, \d+ links?)?\.?$"
    )
    .unwrap();
}

/// Returns true if `line` is `tree`'s trailing summary line (and only that).
fn is_tree_summary(line: &str) -> bool {
    TREE_SUMMARY_RE.is_match(line.trim())
}

pub fn run(args: &[String], verbose: u8) -> Result<i32> {
    if !tool_exists("tree") {
        anyhow::bail!(
            "tree command not found. Install it first:\n\
             - macOS: brew install tree\n\
             - Ubuntu/Debian: sudo apt install tree\n\
             - Fedora/RHEL: sudo dnf install tree\n\
             - Arch: sudo pacman -S tree"
        );
    }

    let mut cmd = resolved_command("tree");

    let show_all = args.iter().any(|a| a == "-a" || a == "--all");
    let has_ignore = args.iter().any(|a| a == "-I" || a.starts_with("--ignore="));

    if !show_all && !has_ignore {
        let ignore_pattern = NOISE_DIRS.join("|");
        cmd.arg("-I").arg(&ignore_pattern);
    }

    for arg in args {
        cmd.arg(arg);
    }

    runner::run_filtered(
        cmd,
        "tree",
        &args.join(" "),
        |raw| {
            let filtered = filter_tree_output(raw);
            if verbose > 0 {
                eprintln!(
                    "Lines: {} → {} ({}% reduction)",
                    raw.lines().count(),
                    filtered.lines().count(),
                    if raw.lines().count() > 0 {
                        100 - (filtered.lines().count() * 100 / raw.lines().count())
                    } else {
                        0
                    }
                );
            }
            filtered
        },
        RunOptions::stdout_only()
            .early_exit_on_failure()
            .no_trailing_newline(),
    )
}

fn filter_tree_output(raw: &str) -> String {
    let lines: Vec<&str> = raw.lines().collect();

    if lines.is_empty() {
        return "\n".to_string();
    }

    let mut filtered_lines = Vec::new();

    for line in lines {
        // Skip leading empty lines
        if line.trim().is_empty() && filtered_lines.is_empty() {
            continue;
        }

        filtered_lines.push(line);
    }

    // Remove trailing empty lines
    while filtered_lines.last().is_some_and(|l| l.trim().is_empty()) {
        filtered_lines.pop();
    }

    // Strip the trailing summary line (e.g., "5 directories, 23 files") — and
    // ONLY that line. tree always emits the summary as its last content line, so
    // checking the tail (not every line) means a legitimate entry whose name
    // merely matches the summary shape can never be dropped from the middle of
    // the tree. The exact-shape regex avoids the old over-matching substring
    // heuristic that silently dropped entries like `director-files.txt`.
    if filtered_lines.last().is_some_and(|l| is_tree_summary(l)) {
        filtered_lines.pop();
        // tree separates the summary from the tree with a blank line; drop it too.
        while filtered_lines.last().is_some_and(|l| l.trim().is_empty()) {
            filtered_lines.pop();
        }
    }

    filtered_lines.join("\n") + "\n"
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_filter_removes_summary() {
        let input = ".\n├── src\n│   └── main.rs\n└── Cargo.toml\n\n2 directories, 3 files\n";
        let output = filter_tree_output(input);
        assert!(!output.contains("directories"));
        assert!(!output.contains("files"));
        assert!(output.contains("main.rs"));
        assert!(output.contains("Cargo.toml"));
    }

    #[test]
    fn test_filter_preserves_structure() {
        let input = ".\n├── src\n│   ├── main.rs\n│   └── lib.rs\n└── tests\n    └── test.rs\n";
        let output = filter_tree_output(input);
        assert!(output.contains("├──"));
        assert!(output.contains("│"));
        assert!(output.contains("└──"));
        assert!(output.contains("main.rs"));
        assert!(output.contains("test.rs"));
    }

    #[test]
    fn test_filter_handles_empty() {
        let input = "";
        let output = filter_tree_output(input);
        assert_eq!(output, "\n");
    }

    #[test]
    fn test_filter_removes_trailing_empty_lines() {
        let input = ".\n├── file.txt\n\n\n";
        let output = filter_tree_output(input);
        assert_eq!(output.matches('\n').count(), 2); // Root + file.txt + final newline
    }

    #[test]
    fn test_filter_summary_variations() {
        // Test different summary formats
        let inputs = vec![
            (".\n└── file.txt\n\n0 directories, 1 file\n", "1 file"),
            (".\n└── file.txt\n\n1 directory, 0 files\n", "1 directory"),
            (".\n└── file.txt\n\n10 directories, 25 files\n", "25 files"),
        ];

        for (input, summary_fragment) in inputs {
            let output = filter_tree_output(input);
            assert!(
                !output.contains(summary_fragment),
                "Should remove summary '{}' from output",
                summary_fragment
            );
            assert!(
                output.contains("file.txt"),
                "Should preserve file.txt in output"
            );
        }
    }

    // Regression: the summary-stripping heuristic must not drop legitimate tree
    // entries whose filename happens to contain both "director" and "file"
    // substrings (e.g. `director-files.txt`, `file_directory.json`). The class of
    // bug here is an over-matching transform that silently strips real content
    // lines the model then reasons over — same family as #6 (grep) and #8 (wc).
    #[test]
    fn test_filename_with_director_and_file_not_dropped() {
        let input = ".\n\
                     ├── director-files.txt\n\
                     ├── file_directory.json\n\
                     ├── normal.rs\n\
                     └── sub\n    \
                     └── files-and-directories.md\n\n\
                     2 directories, 4 files\n";
        let output = filter_tree_output(input);
        assert!(
            output.contains("director-files.txt"),
            "file 'director-files.txt' must survive filtering, got:\n{output}"
        );
        assert!(
            output.contains("file_directory.json"),
            "file 'file_directory.json' must survive filtering, got:\n{output}"
        );
        assert!(
            output.contains("files-and-directories.md"),
            "file 'files-and-directories.md' must survive filtering, got:\n{output}"
        );
        assert!(
            output.contains("normal.rs"),
            "file 'normal.rs' must survive filtering, got:\n{output}"
        );
        // The actual summary line must still be removed.
        assert!(
            !output.contains("2 directories, 4 files"),
            "real summary line should be stripped, got:\n{output}"
        );
    }

    // Regression: a connector line that merely mentions both words in a path
    // segment must not be treated as the summary. Only the trailing
    // `N director(y|ies), M file(s)` line is the summary.
    #[test]
    fn test_summary_only_stripped_at_tail() {
        // No real summary line present; nothing should be dropped.
        let input = ".\n\
                     ├── my directory with files\n\
                     └── data.txt\n";
        let output = filter_tree_output(input);
        assert!(
            output.contains("my directory with files"),
            "entry naming directories and files must survive, got:\n{output}"
        );
        assert!(output.contains("data.txt"), "got:\n{output}");
    }

    // Some `tree` builds append a link count: "N directories, M files, K links".
    // Tail-only matching: even a (contrived) entry literally named like the
    // summary survives when it is NOT the last content line. Only tree's real
    // trailing summary is stripped.
    #[test]
    fn test_summary_shaped_name_in_middle_survives() {
        let input = ".\n\
                     ├── 5 directories, 23 files\n\
                     └── real.txt\n\n\
                     1 directory, 2 files\n";
        let output = filter_tree_output(input);
        assert!(
            output.contains("5 directories, 23 files"),
            "a mid-tree entry shaped like a summary must survive, got:\n{output}"
        );
        assert!(output.contains("real.txt"), "got:\n{output}");
        assert!(
            !output.contains("1 directory, 2 files"),
            "the real trailing summary must still be stripped, got:\n{output}"
        );
    }

    #[test]
    fn test_summary_with_links_variation_stripped() {
        let input = ".\n└── link.txt\n\n3 directories, 5 files, 2 links\n";
        let output = filter_tree_output(input);
        assert!(output.contains("link.txt"), "got:\n{output}");
        assert!(
            !output.contains("3 directories, 5 files, 2 links"),
            "summary-with-links line should be stripped, got:\n{output}"
        );
    }

    #[test]
    fn test_noise_dirs_constant() {
        // Verify NOISE_DIRS contains expected patterns
        assert!(NOISE_DIRS.contains(&"node_modules"));
        assert!(NOISE_DIRS.contains(&".git"));
        assert!(NOISE_DIRS.contains(&"target"));
        assert!(NOISE_DIRS.contains(&"__pycache__"));
        assert!(NOISE_DIRS.contains(&".next"));
        assert!(NOISE_DIRS.contains(&"dist"));
        assert!(NOISE_DIRS.contains(&"build"));
    }
}
