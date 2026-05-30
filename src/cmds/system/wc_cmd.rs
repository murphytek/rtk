/// Compact filter for `wc` — strips redundant paths and alignment padding.
///
/// Compression examples:
/// - `wc file.py`     → `30L 96W 978B`
/// - `wc -l file.py`  → `30`
/// - `wc -w file.py`  → `96`
/// - `wc -c file.py`  → `978`
/// - `wc -l *.py`     → table with common path prefix stripped
use crate::core::runner::{self, RunOptions};
use crate::core::utils::resolved_command;
use anyhow::Result;

pub fn run(args: &[String], verbose: u8) -> Result<i32> {
    let mut cmd = resolved_command("wc");
    for arg in args {
        cmd.arg(arg);
    }

    if verbose > 0 {
        eprintln!("Running: wc {}", args.join(" "));
    }

    let mode = detect_mode(args);
    runner::run_filtered(
        cmd,
        "wc",
        &args.join(" "),
        |stdout| filter_wc_output(stdout, &mode),
        RunOptions::stdout_only(),
    )
}

/// Which columns the user requested
#[derive(Debug, PartialEq)]
enum WcMode {
    /// Default: lines, words, bytes (3 columns)
    Full,
    /// Lines only (-l)
    Lines,
    /// Words only (-w)
    Words,
    /// Bytes only (-c)
    Bytes,
    /// Chars only (-m)
    Chars,
    /// Multiple flags combined — keep compact format
    Mixed,
}

fn detect_mode(args: &[String]) -> WcMode {
    let flags: Vec<&str> = args
        .iter()
        .filter(|a| a.starts_with('-'))
        .map(|s| s.as_str())
        .collect();

    if flags.is_empty() {
        return WcMode::Full;
    }

    // Collect all single-char flags (handles combined flags like -lw)
    let mut has_l = false;
    let mut has_w = false;
    let mut has_c = false;
    let mut has_m = false;
    let mut flag_count = 0;

    for flag in &flags {
        for ch in flag.chars().skip(1) {
            match ch {
                'l' => {
                    has_l = true;
                    flag_count += 1;
                }
                'w' => {
                    has_w = true;
                    flag_count += 1;
                }
                'c' => {
                    has_c = true;
                    flag_count += 1;
                }
                'm' => {
                    has_m = true;
                    flag_count += 1;
                }
                _ => {}
            }
        }
    }

    if flag_count == 0 {
        return WcMode::Full;
    }
    if flag_count > 1 {
        return WcMode::Mixed;
    }

    if has_l {
        WcMode::Lines
    } else if has_w {
        WcMode::Words
    } else if has_c {
        WcMode::Bytes
    } else if has_m {
        WcMode::Chars
    } else {
        WcMode::Full
    }
}

fn filter_wc_output(raw: &str, mode: &WcMode) -> String {
    let lines: Vec<&str> = raw.trim().lines().collect();

    if lines.is_empty() {
        return String::new();
    }

    // Single file (one output line, no "total")
    if lines.len() == 1 {
        return format_single_line(lines[0], mode);
    }

    // Multiple files — compact table
    format_multi_line(&lines, mode)
}

/// Split a wc output line into its leading numeric count columns and the
/// trailing filename (if any).
///
/// `wc` prints `<count> [count...] <filename>` where the filename is emitted
/// verbatim and MAY contain spaces (e.g. `30 my file.txt`). Naive
/// `split_whitespace()` would shatter such names, so callers must split off a
/// known number of count columns and treat the remainder as one opaque name.
///
/// `max_counts` is the most count columns the mode can have. We consume up to
/// that many *leading* tokens that parse as unsigned integers; the first
/// non-numeric token marks the start of the filename, and the filename is
/// returned with its internal spacing preserved.
///
/// Returns `(counts, name)`. `name` is `None` for count-only lines (stdin,
/// or the `total` summary line which the caller handles separately).
fn split_counts_and_name(line: &str, max_counts: usize) -> (Vec<&str>, Option<&str>) {
    let trimmed = line.trim_start();
    let mut counts = Vec::new();
    let mut rest = trimmed;

    while counts.len() < max_counts {
        // Peel one whitespace-delimited token off the front.
        let token_end = rest.find(char::is_whitespace).unwrap_or(rest.len());
        let token = &rest[..token_end];
        if token.is_empty() || token.parse::<u64>().is_err() {
            break;
        }
        counts.push(token);
        rest = rest[token_end..].trim_start();
    }

    let name = if rest.is_empty() { None } else { Some(rest) };
    (counts, name)
}

/// Format a single wc output line (one file or stdin)
fn format_single_line(line: &str, mode: &WcMode) -> String {
    let parts: Vec<&str> = line.split_whitespace().collect();

    match mode {
        WcMode::Lines | WcMode::Words | WcMode::Bytes | WcMode::Chars => {
            // First number is the only requested column
            parts.first().map(|s| s.to_string()).unwrap_or_default()
        }
        WcMode::Full => {
            if parts.len() >= 3 {
                format!("{}L {}W {}B", parts[0], parts[1], parts[2])
            } else {
                line.trim().to_string()
            }
        }
        WcMode::Mixed => {
            // Strip the (possibly space-containing) filename, keep counts only.
            // Mixed can have up to 4 count columns (-lwcm); peel the numeric
            // prefix off the front rather than guessing from the last token,
            // which mis-handles names like `my file.txt`.
            let (counts, _name) = split_counts_and_name(line, 4);
            if counts.is_empty() {
                line.trim().to_string()
            } else {
                counts.join(" ")
            }
        }
    }
}

/// Format multiple files as a compact table
fn format_multi_line(lines: &[&str], mode: &WcMode) -> String {
    let mut result = Vec::new();

    // Max count columns this mode emits (Mixed can be up to -lwcm = 4).
    let max_counts = match mode {
        WcMode::Lines | WcMode::Words | WcMode::Bytes | WcMode::Chars => 1,
        WcMode::Full => 3,
        WcMode::Mixed => 4,
    };

    // Find common directory prefix to shorten paths. Filenames may contain
    // spaces, so split off the numeric prefix and take the verbatim remainder
    // as the name rather than grabbing the last whitespace token.
    let paths: Vec<&str> = lines
        .iter()
        .filter_map(|line| split_counts_and_name(line, max_counts).1)
        .filter(|p| *p != "total")
        .collect();

    let common_prefix = find_common_prefix(&paths);

    for line in lines {
        let (counts, name) = split_counts_and_name(line, max_counts);
        if counts.is_empty() && name.is_none() {
            continue;
        }

        let is_total = name == Some("total");
        let stripped_name = name.map(|n| strip_prefix(n, &common_prefix));

        match mode {
            WcMode::Lines | WcMode::Words | WcMode::Bytes | WcMode::Chars => {
                let count = counts.first().copied().unwrap_or("0");
                if is_total {
                    result.push(format!("Σ {}", count));
                } else if let Some(name) = stripped_name {
                    result.push(format!("{} {}", count, name));
                } else {
                    result.push(count.to_string());
                }
            }
            WcMode::Full => {
                let c0 = counts.first().copied().unwrap_or("0");
                let c1 = counts.get(1).copied().unwrap_or("0");
                let c2 = counts.get(2).copied().unwrap_or("0");
                if is_total {
                    result.push(format!("Σ {}L {}W {}B", c0, c1, c2));
                } else if counts.len() >= 3 {
                    if let Some(name) = stripped_name {
                        result.push(format!("{}L {}W {}B {}", c0, c1, c2, name));
                    } else {
                        result.push(format!("{}L {}W {}B", c0, c1, c2));
                    }
                } else {
                    result.push(line.trim().to_string());
                }
            }
            WcMode::Mixed => {
                if is_total {
                    result.push(format!("Σ {}", counts.join(" ")));
                } else if counts.is_empty() {
                    result.push(line.trim().to_string());
                } else if let Some(name) = stripped_name {
                    result.push(format!("{} {}", counts.join(" "), name));
                } else {
                    result.push(counts.join(" "));
                }
            }
        }
    }

    result.join("\n")
}

/// Find common directory prefix among paths
fn find_common_prefix(paths: &[&str]) -> String {
    if paths.len() <= 1 {
        return String::new();
    }

    let first = paths[0];
    let prefix = if let Some(pos) = first.rfind('/') {
        &first[..=pos]
    } else {
        return String::new();
    };

    if paths.iter().all(|p| p.starts_with(prefix)) {
        return prefix.to_string();
    }

    // Try shorter prefixes by removing right-most segments
    let mut candidate = prefix.to_string();
    while !candidate.is_empty() {
        if paths.iter().all(|p| p.starts_with(&candidate)) {
            return candidate;
        }
        if let Some(pos) = candidate[..candidate.len() - 1].rfind('/') {
            candidate.truncate(pos + 1);
        } else {
            return String::new();
        }
    }
    String::new()
}

/// Strip common prefix from a path
fn strip_prefix<'a>(path: &'a str, prefix: &str) -> &'a str {
    if prefix.is_empty() {
        return path;
    }
    path.strip_prefix(prefix).unwrap_or(path)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_single_file_full() {
        let raw = "      30      96     978 scripts/find_duplicate_attrs.py\n";
        let result = filter_wc_output(raw, &WcMode::Full);
        assert_eq!(result, "30L 96W 978B");
    }

    #[test]
    fn test_single_file_lines_only() {
        let raw = "      30 scripts/find_duplicate_attrs.py\n";
        let result = filter_wc_output(raw, &WcMode::Lines);
        assert_eq!(result, "30");
    }

    #[test]
    fn test_single_file_words_only() {
        let raw = "      96 scripts/find_duplicate_attrs.py\n";
        let result = filter_wc_output(raw, &WcMode::Words);
        assert_eq!(result, "96");
    }

    #[test]
    fn test_stdin_full() {
        let raw = "      30      96     978\n";
        let result = filter_wc_output(raw, &WcMode::Full);
        assert_eq!(result, "30L 96W 978B");
    }

    #[test]
    fn test_stdin_lines() {
        let raw = "      30\n";
        let result = filter_wc_output(raw, &WcMode::Lines);
        assert_eq!(result, "30");
    }

    #[test]
    fn test_multi_file_lines() {
        let raw = "      30 src/main.rs\n      50 src/lib.rs\n      80 total\n";
        let result = filter_wc_output(raw, &WcMode::Lines);
        assert_eq!(result, "30 main.rs\n50 lib.rs\nΣ 80");
    }

    #[test]
    fn test_multi_file_full() {
        let raw = "      30      96     978 src/main.rs\n      50     120    1500 src/lib.rs\n      80     216    2478 total\n";
        let result = filter_wc_output(raw, &WcMode::Full);
        assert_eq!(
            result,
            "30L 96W 978B main.rs\n50L 120W 1500B lib.rs\nΣ 80L 216W 2478B"
        );
    }

    #[test]
    fn test_detect_mode_full() {
        let args: Vec<String> = vec!["file.py".into()];
        assert_eq!(detect_mode(&args), WcMode::Full);
    }

    #[test]
    fn test_detect_mode_lines() {
        let args: Vec<String> = vec!["-l".into(), "file.py".into()];
        assert_eq!(detect_mode(&args), WcMode::Lines);
    }

    #[test]
    fn test_detect_mode_mixed() {
        let args: Vec<String> = vec!["-lw".into(), "file.py".into()];
        assert_eq!(detect_mode(&args), WcMode::Mixed);
    }

    #[test]
    fn test_detect_mode_separate_flags() {
        let args: Vec<String> = vec!["-l".into(), "-w".into(), "file.py".into()];
        assert_eq!(detect_mode(&args), WcMode::Mixed);
    }

    #[test]
    fn test_common_prefix() {
        let paths = vec!["src/main.rs", "src/lib.rs", "src/utils.rs"];
        assert_eq!(find_common_prefix(&paths), "src/");
    }

    #[test]
    fn test_no_common_prefix() {
        let paths = vec!["main.rs", "lib.rs"];
        assert_eq!(find_common_prefix(&paths), "");
    }

    #[test]
    fn test_deep_common_prefix() {
        let paths = vec!["src/cmd/wc.rs", "src/cmd/ls.rs"];
        assert_eq!(find_common_prefix(&paths), "src/cmd/");
    }

    #[test]
    fn test_empty() {
        let raw = "";
        let result = filter_wc_output(raw, &WcMode::Full);
        assert_eq!(result, "");
    }

    // --- filenames containing spaces (regression: split_whitespace mangled them) ---
    //
    // GNU wc prints the filename verbatim, so `wc -l "my file.txt"` emits
    // `3 my file.txt`. The model reads this output and decides which file to
    // act on; silently dropping part of the name (or chopping it to the last
    // path segment) corrupts that decision. These tests pin the verbatim name.

    #[test]
    fn test_single_file_full_name_with_spaces() {
        let raw = "      30      96     978 my file.txt\n";
        let result = filter_wc_output(raw, &WcMode::Full);
        // Full single-file mode drops the name by design, but it must not
        // mis-parse the counts when extra space-separated tokens follow.
        assert_eq!(result, "30L 96W 978B");
    }

    #[test]
    fn test_multi_file_lines_name_with_spaces() {
        let raw = "      30 my file.txt\n      50 other one.txt\n      80 total\n";
        let result = filter_wc_output(raw, &WcMode::Lines);
        // No common prefix, so the full name must survive intact.
        assert_eq!(result, "30 my file.txt\n50 other one.txt\nΣ 80");
    }

    #[test]
    fn test_multi_file_full_name_with_spaces() {
        let raw = "      30      96     978 my file.txt\n      50     120    1500 other one.txt\n      80     216    2478 total\n";
        let result = filter_wc_output(raw, &WcMode::Full);
        assert_eq!(
            result,
            "30L 96W 978B my file.txt\n50L 120W 1500B other one.txt\nΣ 80L 216W 2478B"
        );
    }

    #[test]
    fn test_multi_file_full_name_with_spaces_common_prefix() {
        // Common dir prefix is stripped, but the spaced basename stays whole.
        let raw = "      30      96     978 src/my file.txt\n      50     120    1500 src/other one.txt\n      80     216    2478 total\n";
        let result = filter_wc_output(raw, &WcMode::Full);
        assert_eq!(
            result,
            "30L 96W 978B my file.txt\n50L 120W 1500B other one.txt\nΣ 80L 216W 2478B"
        );
    }

    #[test]
    fn test_multi_file_lines_common_prefix_with_spaces() {
        let raw = "      30 src/my file.txt\n      50 src/other one.txt\n      80 total\n";
        let result = filter_wc_output(raw, &WcMode::Lines);
        assert_eq!(result, "30 my file.txt\n50 other one.txt\nΣ 80");
    }

    #[test]
    fn test_mixed_single_name_with_spaces() {
        // -lw on a spaced filename: two count columns then the verbatim name.
        let raw = "      30      96 my file.txt\n";
        let result = filter_wc_output(raw, &WcMode::Mixed);
        // Mixed single-line strips the path entirely, keeping only the counts;
        // it must not leave a dangling fragment of the filename.
        assert_eq!(result, "30 96");
    }

    // --- split_counts_and_name helper contract ---

    #[test]
    fn test_split_counts_single_column() {
        let (counts, name) = split_counts_and_name("      30 my file.txt", 1);
        assert_eq!(counts, vec!["30"]);
        assert_eq!(name, Some("my file.txt"));
    }

    #[test]
    fn test_split_counts_full_columns() {
        let (counts, name) = split_counts_and_name("  30  96  978 my file.txt", 3);
        assert_eq!(counts, vec!["30", "96", "978"]);
        assert_eq!(name, Some("my file.txt"));
    }

    #[test]
    fn test_split_counts_stdin_no_name() {
        // No filename (stdin): all tokens are counts, name is None.
        let (counts, name) = split_counts_and_name("      30      96     978", 3);
        assert_eq!(counts, vec!["30", "96", "978"]);
        assert_eq!(name, None);
    }

    #[test]
    fn test_split_counts_stops_at_count_cap() {
        // Even if more numeric-looking tokens follow, never consume more than
        // the mode's column count — the extras belong to the filename.
        let (counts, name) = split_counts_and_name("30 123 456", 1);
        assert_eq!(counts, vec!["30"]);
        assert_eq!(name, Some("123 456"));
    }

    #[test]
    fn test_split_counts_total_line() {
        let (counts, name) = split_counts_and_name("      80 total", 1);
        assert_eq!(counts, vec!["80"]);
        assert_eq!(name, Some("total"));
    }

    #[test]
    fn test_split_counts_trailing_space_in_name() {
        // Tabs/extra spaces between counts and a spaced name must not leak
        // into either side.
        let (counts, name) = split_counts_and_name("  5  10 dir/a b.txt", 2);
        assert_eq!(counts, vec!["5", "10"]);
        assert_eq!(name, Some("dir/a b.txt"));
    }

    #[test]
    fn test_mixed_multi_name_with_spaces() {
        let raw = "      30      96 my file.txt\n      50     120 other one.txt\n      80     216 total\n";
        let result = filter_wc_output(raw, &WcMode::Mixed);
        assert_eq!(
            result,
            "30 96 my file.txt\n50 120 other one.txt\nΣ 80 216"
        );
    }
}
