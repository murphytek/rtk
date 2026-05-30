//! Reads source files with optional language-aware filtering to strip boilerplate.

use crate::core::filter::{self, FilterLevel, Language};
use crate::core::tracking;
use anyhow::{Context, Result};
use std::fs;
use std::path::Path;

pub fn run(
    file: &Path,
    level: FilterLevel,
    max_lines: Option<usize>,
    tail_lines: Option<usize>,
    line_numbers: bool,
    verbose: u8,
) -> Result<()> {
    let timer = tracking::TimedExecution::start();

    if verbose > 0 {
        eprintln!("Reading: {} (filter: {})", file.display(), level);
    }

    // Read file content
    let content = fs::read_to_string(file)
        .with_context(|| format!("Failed to read file: {}", file.display()))?;

    // Detect language from extension
    let lang = file
        .extension()
        .and_then(|e| e.to_str())
        .map(Language::from_extension)
        .unwrap_or(Language::Unknown);

    if verbose > 1 {
        eprintln!("Detected language: {:?}", lang);
    }

    // Apply filter
    let filter = filter::get_filter(level);
    let mut filtered = filter.filter(&content, &lang);

    // Safety: if filter emptied a non-empty file, fall back to raw content
    if filtered.trim().is_empty() && !content.trim().is_empty() {
        eprintln!(
            "rtk: warning: filter produced empty output for {} ({} bytes), showing raw content",
            file.display(),
            content.len()
        );
        filtered = content.clone();
    }

    if verbose > 0 {
        let original_lines = content.lines().count();
        let filtered_lines = filtered.lines().count();
        let reduction = if original_lines > 0 {
            ((original_lines - filtered_lines) as f64 / original_lines as f64) * 100.0
        } else {
            0.0
        };
        eprintln!(
            "Lines: {} -> {} ({:.1}% reduction)",
            original_lines, filtered_lines, reduction
        );
    }

    let line_offset;
    (filtered, line_offset) = apply_line_window(&filtered, max_lines, tail_lines, &lang);

    let rtk_output = if line_numbers {
        format_with_line_numbers(&filtered, line_offset)
    } else {
        filtered.clone()
    };
    print!("{}", rtk_output);
    timer.track(
        &format!("cat {}", file.display()),
        "rtk read",
        &content,
        &rtk_output,
    );
    Ok(())
}

pub fn run_stdin(
    level: FilterLevel,
    max_lines: Option<usize>,
    tail_lines: Option<usize>,
    line_numbers: bool,
    verbose: u8,
) -> Result<()> {
    use std::io::{self, Read as IoRead};

    let timer = tracking::TimedExecution::start();

    if verbose > 0 {
        eprintln!("Reading from stdin (filter: {})", level);
    }

    // Read from stdin
    let mut content = String::new();
    io::stdin()
        .lock()
        .read_to_string(&mut content)
        .context("Failed to read from stdin")?;

    // No file extension, so use Unknown language
    let lang = Language::Unknown;

    if verbose > 1 {
        eprintln!("Language: {:?} (stdin has no extension)", lang);
    }

    // Apply filter
    let filter = filter::get_filter(level);
    let mut filtered = filter.filter(&content, &lang);

    if verbose > 0 {
        let original_lines = content.lines().count();
        let filtered_lines = filtered.lines().count();
        let reduction = if original_lines > 0 {
            ((original_lines - filtered_lines) as f64 / original_lines as f64) * 100.0
        } else {
            0.0
        };
        eprintln!(
            "Lines: {} -> {} ({:.1}% reduction)",
            original_lines, filtered_lines, reduction
        );
    }

    let line_offset;
    (filtered, line_offset) = apply_line_window(&filtered, max_lines, tail_lines, &lang);

    let rtk_output = if line_numbers {
        format_with_line_numbers(&filtered, line_offset)
    } else {
        filtered.clone()
    };
    print!("{}", rtk_output);

    timer.track("cat - (stdin)", "rtk read -", &content, &rtk_output);
    Ok(())
}

/// Format `content` with a `│`-separated line-number gutter.
///
/// `start` is the 1-based line number assigned to the first line of `content`.
/// When a tail window has been applied, `start` is the original file position of
/// the first surviving line, so the numbers a reading model sees point at the
/// real lines in the file rather than restarting at 1 (which would silently
/// mislabel the content).
fn format_with_line_numbers(content: &str, start: usize) -> String {
    let lines: Vec<&str> = content.lines().collect();
    // Width is sized for the largest number actually printed (start + len - 1),
    // not just the line count, so a high tail offset still aligns.
    let last = start + lines.len().saturating_sub(1);
    let width = last.max(1).to_string().len();
    let mut out = String::new();
    for (i, line) in lines.iter().enumerate() {
        out.push_str(&format!("{:>width$} │ {}\n", start + i, line, width = width));
    }
    out
}

/// Apply the `--tail-lines` / `--max-lines` window to already-filtered content.
///
/// Returns the windowed text together with the 1-based line number that the
/// first emitted line had in `content`. For a tail window that offset is
/// `total_lines - tail + 1`; in every other case it is `1`.
fn apply_line_window(
    content: &str,
    max_lines: Option<usize>,
    tail_lines: Option<usize>,
    lang: &Language,
) -> (String, usize) {
    if let Some(tail) = tail_lines {
        if tail == 0 {
            return (String::new(), 1);
        }
        let lines: Vec<&str> = content.lines().collect();
        let start = lines.len().saturating_sub(tail);
        let mut result = lines[start..].join("\n");
        if content.ends_with('\n') {
            result.push('\n');
        }
        // `start` is a 0-based index; line numbers are 1-based.
        return (result, start + 1);
    }

    if let Some(max) = max_lines {
        // smart_truncate keeps a non-contiguous selection of lines starting at
        // line 1, so the gutter stays 1-based.
        return (filter::smart_truncate(content, max, lang), 1);
    }

    (content.to_string(), 1)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use tempfile::NamedTempFile;

    #[test]
    fn test_read_rust_file() -> Result<()> {
        let mut file = NamedTempFile::with_suffix(".rs")?;
        writeln!(
            file,
            r#"// Comment
fn main() {{
    println!("Hello");
}}"#
        )?;

        // Just verify it doesn't panic
        run(file.path(), FilterLevel::Minimal, None, None, false, 0)?;
        Ok(())
    }

    #[test]
    fn test_stdin_support_signature() {
        // Test that run_stdin has correct signature and compiles
        // We don't actually run it because it would hang waiting for stdin
        // Compile-time verification that the function exists with correct signature
    }

    #[test]
    fn test_apply_line_window_tail_lines() {
        let input = "a\nb\nc\nd\n";
        let (output, offset) = apply_line_window(input, None, Some(2), &Language::Unknown);
        assert_eq!(output, "c\nd\n");
        // c is the 3rd line of the 4-line input.
        assert_eq!(offset, 3);
    }

    #[test]
    fn test_apply_line_window_tail_lines_no_trailing_newline() {
        let input = "a\nb\nc\nd";
        let (output, offset) = apply_line_window(input, None, Some(2), &Language::Unknown);
        assert_eq!(output, "c\nd");
        assert_eq!(offset, 3);
    }

    #[test]
    fn test_apply_line_window_max_lines_still_works() {
        let input = "a\nb\nc\nd\n";
        let (output, offset) = apply_line_window(input, Some(2), None, &Language::Unknown);
        assert!(output.starts_with("a\n"));
        assert!(output.contains("more lines"));
        // max-lines keeps from the top, so the gutter starts at 1.
        assert_eq!(offset, 1);
    }

    #[test]
    fn test_apply_line_window_no_window_offset_is_one() {
        let input = "a\nb\nc\n";
        let (output, offset) = apply_line_window(input, None, None, &Language::Unknown);
        assert_eq!(output, "a\nb\nc\n");
        assert_eq!(offset, 1);
    }

    #[test]
    fn test_apply_line_window_tail_zero_is_empty() {
        let input = "a\nb\nc\n";
        let (output, offset) = apply_line_window(input, None, Some(0), &Language::Unknown);
        assert_eq!(output, "");
        assert_eq!(offset, 1);
    }

    #[test]
    fn test_apply_line_window_tail_larger_than_content() {
        // Asking for more tail lines than exist keeps everything, offset stays 1.
        let input = "a\nb\n";
        let (output, offset) = apply_line_window(input, None, Some(10), &Language::Unknown);
        assert_eq!(output, "a\nb\n");
        assert_eq!(offset, 1);
    }

    #[test]
    fn test_format_line_numbers_default_offset() {
        let out = format_with_line_numbers("alpha\nbravo\n", 1);
        assert_eq!(out, "1 │ alpha\n2 │ bravo\n");
    }

    // REGRESSION: tail + line numbers must report the ORIGINAL file positions,
    // not restart at 1. Reverting the offset wiring makes this fail (the gutter
    // would read `1`/`2` for lines that are really 4/5), which is the
    // output-mangling class — a model would cite the wrong line numbers.
    #[test]
    fn test_format_line_numbers_with_tail_offset() {
        // A 5-line file, tail 2 -> last two lines are file lines 4 and 5.
        let input = "l1\nl2\nl3\nl4\nl5\n";
        let (windowed, offset) = apply_line_window(input, None, Some(2), &Language::Unknown);
        assert_eq!(offset, 4);
        let out = format_with_line_numbers(&windowed, offset);
        assert_eq!(out, "4 │ l4\n5 │ l5\n");
        // The mislabeled, pre-fix output must NOT be produced.
        assert!(
            !out.starts_with("1 │ l4"),
            "tail window must not renumber from 1"
        );
    }

    // Gutter width is sized for the largest number actually printed, so a tail
    // window whose offset crosses a digit boundary stays aligned.
    #[test]
    fn test_format_line_numbers_width_accounts_for_offset() {
        // Lines 9 and 10 of the file: widths 1 and 2 -> pad to width 2.
        let mut input = String::new();
        for i in 1..=10 {
            input.push_str(&format!("line{}\n", i));
        }
        let (windowed, offset) = apply_line_window(&input, None, Some(2), &Language::Unknown);
        assert_eq!(offset, 9);
        let out = format_with_line_numbers(&windowed, offset);
        assert_eq!(out, " 9 │ line9\n10 │ line10\n");
    }

    #[test]
    fn test_format_line_numbers_empty_content() {
        // Empty content must not panic on the width computation.
        assert_eq!(format_with_line_numbers("", 1), "");
    }

    fn rtk_bin() -> std::path::PathBuf {
        std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("target")
            .join("debug")
            .join("rtk")
    }

    #[test]
    #[ignore]
    fn test_read_two_valid_files_concatenated() {
        let bin = rtk_bin();
        assert!(bin.exists(), "Run `cargo build` first");

        let mut f1 = NamedTempFile::with_suffix(".txt").unwrap();
        let mut f2 = NamedTempFile::with_suffix(".txt").unwrap();
        writeln!(f1, "alpha\nbravo").unwrap();
        writeln!(f2, "charlie\ndelta").unwrap();

        let output = std::process::Command::new(&bin)
            .args(["read", &f1.path().to_string_lossy(), &f2.path().to_string_lossy()])
            .output()
            .expect("failed to run rtk read");

        assert!(output.status.success());
        let stdout = String::from_utf8_lossy(&output.stdout);
        assert!(stdout.contains("alpha"), "first file content missing");
        assert!(stdout.contains("charlie"), "second file content missing");
    }

    #[test]
    #[ignore]
    fn test_read_valid_and_nonexistent() {
        let bin = rtk_bin();
        assert!(bin.exists(), "Run `cargo build` first");

        let mut f1 = NamedTempFile::with_suffix(".txt").unwrap();
        writeln!(f1, "valid content").unwrap();

        let output = std::process::Command::new(&bin)
            .args(["read", &f1.path().to_string_lossy(), "/tmp/rtk_nonexistent_file.txt"])
            .output()
            .expect("failed to run rtk read");

        assert!(!output.status.success(), "should exit non-zero on missing file");
        let stdout = String::from_utf8_lossy(&output.stdout);
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(stdout.contains("valid content"), "valid file should still be printed");
        assert!(stderr.contains("rtk_nonexistent_file"), "should report missing file on stderr");
    }

    #[test]
    #[ignore]
    fn test_read_stdin_dedup_warning() {
        let bin = rtk_bin();
        assert!(bin.exists(), "Run `cargo build` first");

        let output = std::process::Command::new(&bin)
            .args(["read", "-", "-"])
            .stdin(std::process::Stdio::piped())
            .output()
            .expect("failed to run rtk read");

        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(
            stderr.contains("stdin specified more than once"),
            "should warn about duplicate stdin, got stderr: {}",
            stderr
        );
    }
}
