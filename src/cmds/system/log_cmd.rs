//! Deduplicates repeated log lines and shows counts instead.

use crate::core::tracking;
use crate::core::truncate::{reduced, CAP_WARNINGS};
use anyhow::Result;
use lazy_static::lazy_static;
use regex::Regex;
use std::collections::HashMap;
use std::fs;
use std::io::{self, BufRead};
use std::path::Path;

lazy_static! {
    static ref TIMESTAMP_RE: Regex =
        Regex::new(r"^\d{4}[-/]\d{2}[-/]\d{2}[T ]\d{2}:\d{2}:\d{2}[.,]?\d*\s*").unwrap();
    static ref UUID_RE: Regex =
        Regex::new(r"[0-9a-fA-F]{8}-[0-9a-fA-F]{4}-[0-9a-fA-F]{4}-[0-9a-fA-F]{4}-[0-9a-fA-F]{12}")
            .unwrap();
    static ref HEX_RE: Regex = Regex::new(r"0x[0-9a-fA-F]+").unwrap();
    static ref NUM_RE: Regex = Regex::new(r"\b\d{4,}\b").unwrap();
    static ref PATH_RE: Regex = Regex::new(r"/[\w./\-]+").unwrap();
}

/// Filter and deduplicate log output
pub fn run_file(file: &Path, verbose: u8) -> Result<()> {
    let timer = tracking::TimedExecution::start();

    if verbose > 0 {
        eprintln!("Analyzing log: {}", file.display());
    }

    let content = fs::read_to_string(file)?;
    let result = analyze_logs(&content);
    println!("{}", result);
    timer.track(
        &format!("cat {}", file.display()),
        "rtk log",
        &content,
        &result,
    );
    Ok(())
}

/// Filter logs from stdin
pub fn run_stdin(_verbose: u8) -> Result<()> {
    let timer = tracking::TimedExecution::start();

    let mut content = String::new();
    let stdin = io::stdin();
    for line in stdin.lock().lines() {
        content.push_str(&line?);
        content.push('\n');
    }

    let result = analyze_logs(&content);
    println!("{}", result);

    timer.track("log (stdin)", "rtk log (stdin)", &content, &result);

    Ok(())
}

/// For use by other modules
pub fn run_stdin_str(content: &str) -> String {
    analyze_logs(content)
}

fn analyze_logs(content: &str) -> String {
    let mut result = Vec::new();
    let mut error_counts: HashMap<String, usize> = HashMap::new();
    let mut warn_counts: HashMap<String, usize> = HashMap::new();
    let mut info_counts: HashMap<String, usize> = HashMap::new();
    let mut unique_errors: Vec<String> = Vec::new();
    let mut unique_warnings: Vec<String> = Vec::new();

    // Use module-level lazy_static regexes for normalization

    for line in content.lines() {
        let line_lower = line.to_lowercase();

        // Normalize for deduplication
        let normalized =
            normalize_log_line(line, &TIMESTAMP_RE, &UUID_RE, &HEX_RE, &NUM_RE, &PATH_RE);

        // Categorize. The error bucket also covers severity labels above ERROR
        // (CRITICAL, FATAL, ALERT, EMERGENCY, SEVERE, PANIC) — these are the most
        // important lines in a log and were previously dropped as noise when they
        // didn't literally contain "error".
        if line_lower.contains("error")
            || line_lower.contains("fatal")
            || line_lower.contains("panic")
            || line_lower.contains("critical")
            || line_lower.contains("alert")
            || line_lower.contains("emerg")
            || line_lower.contains("severe")
        {
            let count = error_counts.entry(normalized.clone()).or_insert(0);
            if *count == 0 {
                unique_errors.push(line.to_string());
            }
            *count += 1;
        } else if line_lower.contains("warn") || line_lower.contains("notice") {
            let count = warn_counts.entry(normalized.clone()).or_insert(0);
            if *count == 0 {
                unique_warnings.push(line.to_string());
            }
            *count += 1;
        } else if line_lower.contains("info") {
            *info_counts.entry(normalized).or_insert(0) += 1;
        }
    }

    // Summary
    let total_errors: usize = error_counts.values().sum();
    let total_warnings: usize = warn_counts.values().sum();
    let total_info: usize = info_counts.values().sum();

    result.push("Log Summary".to_string());
    result.push(format!(
        "   [error] {} errors ({} unique)",
        total_errors,
        error_counts.len()
    ));
    result.push(format!(
        "   [warn] {} warnings ({} unique)",
        total_warnings,
        warn_counts.len()
    ));
    result.push(format!("   [info] {} info messages", total_info));
    result.push(String::new());

    // Errors with counts
    if !unique_errors.is_empty() {
        result.push("[ERRORS]".to_string());

        // Sort by count
        let mut error_list: Vec<_> = error_counts.iter().collect();
        error_list.sort_by(|a, b| b.1.cmp(a.1));

        const MAX_LOG_ERRORS: usize = CAP_WARNINGS;
        for (normalized, count) in error_list.iter().take(MAX_LOG_ERRORS) {
            // Find original message
            let original = unique_errors
                .iter()
                .find(|e| {
                    &normalize_log_line(e, &TIMESTAMP_RE, &UUID_RE, &HEX_RE, &NUM_RE, &PATH_RE)
                        == *normalized
                })
                .map(|s| s.as_str())
                .unwrap_or(normalized);

            let truncated = truncate_message(original);

            if **count > 1 {
                result.push(format!("   [×{}] {}", count, truncated));
            } else {
                result.push(format!("   {}", truncated));
            }
        }

        if error_list.len() > MAX_LOG_ERRORS {
            result.push(format!(
                "   ... +{} more unique errors",
                error_list.len() - MAX_LOG_ERRORS
            ));
        }
        result.push(String::new());
    }

    // Warnings with counts
    if !unique_warnings.is_empty() {
        result.push("[WARNINGS]".to_string());

        let mut warn_list: Vec<_> = warn_counts.iter().collect();
        warn_list.sort_by(|a, b| b.1.cmp(a.1));

        // warnings are lower severity than errors — show fewer.
        const MAX_LOG_WARNS: usize = reduced(CAP_WARNINGS, 5);
        for (normalized, count) in warn_list.iter().take(MAX_LOG_WARNS) {
            let original = unique_warnings
                .iter()
                .find(|w| {
                    &normalize_log_line(w, &TIMESTAMP_RE, &UUID_RE, &HEX_RE, &NUM_RE, &PATH_RE)
                        == *normalized
                })
                .map(|s| s.as_str())
                .unwrap_or(normalized);

            let truncated = truncate_message(original);

            if **count > 1 {
                result.push(format!("   [×{}] {}", count, truncated));
            } else {
                result.push(format!("   {}", truncated));
            }
        }

        if warn_list.len() > MAX_LOG_WARNS {
            result.push(format!(
                "   ... +{} more unique warnings",
                warn_list.len() - MAX_LOG_WARNS
            ));
        }
    }

    result.join("\n")
}

fn normalize_log_line(
    line: &str,
    timestamp_re: &Regex,
    uuid_re: &Regex,
    hex_re: &Regex,
    num_re: &Regex,
    path_re: &Regex,
) -> String {
    let mut normalized = timestamp_re.replace_all(line, "").to_string();
    normalized = uuid_re.replace_all(&normalized, "<UUID>").to_string();
    normalized = hex_re.replace_all(&normalized, "<HEX>").to_string();
    normalized = num_re.replace_all(&normalized, "<NUM>").to_string();
    normalized = path_re.replace_all(&normalized, "<PATH>").to_string();
    normalized.trim().to_string()
}

/// Cap a single displayed log message at 100 characters, appending `...` only
/// when content is actually dropped.
///
/// The gate and the cut must use the SAME unit. Gating on `str::len()` (bytes)
/// while cutting with `chars().take(..)` (chars) makes any message that is
/// short in chars but long in bytes — i.e. multi-byte UTF-8 (CJK, Thai, emoji,
/// accented text) — falsely enter the truncate branch: `chars().take(97)`
/// returns the whole string and a misleading `...` gets appended, signalling a
/// cut that never happened. Measuring in chars on both sides keeps the marker
/// honest.
fn truncate_message(original: &str) -> String {
    if original.chars().count() > 100 {
        let t: String = original.chars().take(97).collect();
        format!("{}...", t)
    } else {
        original.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_analyze_logs() {
        let logs = r#"
2024-01-01 10:00:00 ERROR: Connection failed to /api/server
2024-01-01 10:00:01 ERROR: Connection failed to /api/server
2024-01-01 10:00:02 ERROR: Connection failed to /api/server
2024-01-01 10:00:03 WARN: Retrying connection
2024-01-01 10:00:04 INFO: Connected
"#;
        let result = analyze_logs(logs);
        assert!(result.contains("×3"));
        assert!(result.contains("ERRORS"));
    }

    #[test]
    fn test_analyze_logs_extended_severity_keywords() {
        let logs = "2024-01-01 10:00:00 CRITICAL: disk full\n\
                    2024-01-01 10:00:01 ALERT: memory pressure\n\
                    2024-01-01 10:00:02 emerg: system shutdown imminent\n\
                    2024-01-01 10:00:03 SEVERE: data corruption detected\n\
                    2024-01-01 10:00:04 notice: config reloaded\n";
        let result = analyze_logs(logs);
        assert!(result.contains("ERRORS"), "critical/alert/emerg/severe should count as errors");
        assert!(result.contains("WARNINGS"), "notice should count as warning");
    }

    #[test]
    fn test_analyze_logs_multibyte() {
        let logs = format!(
            "2024-01-01 10:00:00 ERROR: {} connection failed\n\
             2024-01-01 10:00:01 WARN: {} retry attempt\n",
            "ข้อผิดพลาด".repeat(15),
            "คำเตือน".repeat(15)
        );
        let result = analyze_logs(&logs);
        // Should not panic even with very long multi-byte messages
        assert!(result.contains("ERRORS"));
    }

    // --- truncate_message: gate and cut must agree on unit (chars, not bytes) ---

    #[test]
    fn truncate_message_keeps_short_ascii_untouched() {
        let msg = "error: connection refused";
        assert_eq!(truncate_message(msg), msg);
        assert!(!truncate_message(msg).ends_with("..."));
    }

    #[test]
    fn truncate_message_cuts_long_ascii_to_97_plus_ellipsis() {
        let msg = "e".repeat(150);
        let out = truncate_message(&msg);
        assert!(out.ends_with("..."));
        // 97 retained chars + the 3-char ellipsis marker.
        assert_eq!(out.chars().count(), 100);
        assert_eq!(&out[..97], &"e".repeat(97));
    }

    #[test]
    fn truncate_message_does_not_falsely_truncate_short_multibyte() {
        // 50 chars, but ~138 bytes (each Thai char is 3 bytes). Byte length
        // exceeds 100 while char length is well under it. The OLD byte-gated
        // logic entered the truncate branch, took all 50 chars, and appended a
        // bogus "..."; this asserts the message is now returned verbatim.
        let msg = format!("error {}", "ก".repeat(44)); // 50 chars, 138 bytes
        assert!(msg.len() > 100, "precondition: byte length must exceed 100");
        assert!(msg.chars().count() <= 100, "precondition: char count under cap");

        let out = truncate_message(&msg);
        assert!(
            !out.ends_with("..."),
            "short multi-byte message must not gain a spurious ellipsis: {out:?}"
        );
        assert_eq!(out, msg, "no characters should be dropped");
    }

    #[test]
    fn truncate_message_cuts_long_multibyte_on_char_boundary() {
        // 120 Thai chars: over the 100-char cap, so it SHOULD truncate — and the
        // cut must land on a char boundary (no panic, valid UTF-8 out).
        let msg = "ก".repeat(120);
        let out = truncate_message(&msg);
        assert!(out.ends_with("..."));
        assert_eq!(out.chars().count(), 100); // 97 + "..."
        assert_eq!(out.chars().take(97).collect::<String>(), "ก".repeat(97));
    }

    #[test]
    fn analyze_logs_no_spurious_ellipsis_on_short_multibyte_error() {
        // End-to-end: a short multi-byte ERROR line must render without a "...".
        let logs = format!("2024-01-01 10:00:00 error {}\n", "ก".repeat(44));
        let result = analyze_logs(&logs);
        assert!(result.contains("ERRORS"));
        assert!(
            !result.contains("..."),
            "rendered output must not contain a bogus truncation marker:\n{result}"
        );
    }
}
