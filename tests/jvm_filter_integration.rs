//! Integration tests for JVM filter functions against realistic captured fixtures.
//!
//! Each test:
//! - Reads a fixture file from tests/fixtures/jvm/
//! - Runs it through the appropriate filter function (via tests/common/mod.rs)
//! - Asserts filtered output is at least 40% smaller than raw input (a deliberately
//!   conservative floor — fixtures regularly hit 70-90% in practice; the threshold
//!   only fails when a regression cuts compression in half)
//! - Asserts critical signal lines are preserved (errors, BUILD result)

mod common;

const FIXTURES: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/jvm");

// ---------------------------------------------------------------------------
// Helper
// ---------------------------------------------------------------------------

fn fixture(name: &str) -> String {
    let path = format!("{}/{}", FIXTURES, name);
    std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("cannot read {path}: {e}"))
}

/// Returns true when the filtered output is at least `pct`% smaller than raw.
#[allow(dead_code)]
fn reduced_by_at_least(raw: &str, filtered: &str, pct: u8) -> bool {
    let raw_len = raw.len();
    if raw_len == 0 {
        return true;
    }
    let filtered_len = filtered.len();
    let reduction = (raw_len - filtered_len.min(raw_len)) as f64 / raw_len as f64;
    reduction >= (pct as f64 / 100.0)
}

// ---------------------------------------------------------------------------
// 1. mvn_clean_compile_with_pgp.txt → filter_mvn_build
// ---------------------------------------------------------------------------

#[test]
fn test_mvn_compile_with_pgp_reduces_by_40pct() {
    let raw = fixture("mvn_clean_compile_with_pgp.txt");
    let filtered = common::filter_mvn_build(&raw);

    assert!(
        reduced_by_at_least(&raw, &filtered, 40),
        "expected ≥40% reduction, got:\nraw={} bytes, filtered={} bytes\n---\n{}",
        raw.len(),
        filtered.len(),
        filtered
    );
}

#[test]
fn test_mvn_compile_with_pgp_preserves_build_success() {
    let raw = fixture("mvn_clean_compile_with_pgp.txt");
    let filtered = common::filter_mvn_build(&raw);

    assert!(
        filtered.contains("BUILD SUCCESS"),
        "BUILD SUCCESS must be preserved, got:\n{}",
        filtered
    );
}

#[test]
fn test_mvn_compile_with_pgp_strips_pgp_noise() {
    let raw = fixture("mvn_clean_compile_with_pgp.txt");
    let filtered = common::filter_mvn_build(&raw);

    assert!(
        !filtered.contains("Key server(s)"),
        "PGP config noise should be stripped, got:\n{}",
        filtered
    );
    assert!(
        !filtered.contains("Copying 3 resources"),
        "resource-copy noise should be stripped"
    );
    assert!(
        !filtered.contains("Starting audit"),
        "checkstyle audit lines should be stripped"
    );
    assert!(
        !filtered.contains("UTF-8' encoding"),
        "encoding notice should be stripped"
    );
}

// ---------------------------------------------------------------------------
// 2. mvn_test_failure.txt → filter_mvn_test
// ---------------------------------------------------------------------------

#[test]
fn test_mvn_test_failure_reduces_by_40pct() {
    let raw = fixture("mvn_test_failure.txt");
    let filtered = common::filter_mvn_test(&raw);

    assert!(
        reduced_by_at_least(&raw, &filtered, 40),
        "expected ≥40% reduction, raw={} filtered={}\n{}",
        raw.len(),
        filtered.len(),
        filtered
    );
}

#[test]
fn test_mvn_test_failure_preserves_error_and_build_failure() {
    let raw = fixture("mvn_test_failure.txt");
    let filtered = common::filter_mvn_test(&raw);

    assert!(
        filtered.contains("BUILD FAILURE"),
        "BUILD FAILURE must be preserved, got:\n{}",
        filtered
    );
    assert!(
        filtered.contains("[ERROR]"),
        "ERROR lines must be preserved, got:\n{}",
        filtered
    );
}

#[test]
fn test_mvn_test_failure_preserves_tests_run_summary() {
    let raw = fixture("mvn_test_failure.txt");
    let filtered = common::filter_mvn_test(&raw);

    assert!(
        filtered.contains("Tests run:"),
        "Tests run: summary must be preserved, got:\n{}",
        filtered
    );
}

// ---------------------------------------------------------------------------
// 3. gradle_test_with_spotbugs_flood.txt → filter_gradle_test
// ---------------------------------------------------------------------------

#[test]
fn test_gradle_spotbugs_reduces_by_40pct() {
    let raw = fixture("gradle_test_with_spotbugs_flood.txt");
    let filtered = common::filter_gradle_test(&raw);

    assert!(
        reduced_by_at_least(&raw, &filtered, 40),
        "expected ≥40% reduction, raw={} filtered={}\n{}",
        raw.len(),
        filtered.len(),
        filtered
    );
}

#[test]
fn test_gradle_spotbugs_preserves_build_failed_and_task_failed() {
    let raw = fixture("gradle_test_with_spotbugs_flood.txt");
    let filtered = common::filter_gradle_test(&raw);

    assert!(
        filtered.contains("BUILD FAILED"),
        "BUILD FAILED must be preserved, got:\n{}",
        filtered
    );
    assert!(
        filtered.contains(":app:test FAILED"),
        "> Task :app:test FAILED must be preserved, got:\n{}",
        filtered
    );
}

#[test]
fn test_gradle_spotbugs_strips_up_to_date_and_progress() {
    let raw = fixture("gradle_test_with_spotbugs_flood.txt");
    let filtered = common::filter_gradle_test(&raw);

    assert!(
        !filtered.contains("UP-TO-DATE"),
        "UP-TO-DATE task lines should be stripped, got:\n{}",
        filtered
    );
    assert!(
        !filtered.contains("EXECUTING"),
        "progress EXECUTING lines should be stripped, got:\n{}",
        filtered
    );
}

// ---------------------------------------------------------------------------
// 4. ant_compile.txt → filter_ant_build
// ---------------------------------------------------------------------------

#[test]
fn test_ant_compile_reduces_by_40pct() {
    let raw = fixture("ant_compile.txt");
    let filtered = common::filter_ant_build(&raw);

    assert!(
        reduced_by_at_least(&raw, &filtered, 40),
        "expected ≥40% reduction, raw={} filtered={}\n{}",
        raw.len(),
        filtered.len(),
        filtered
    );
}

#[test]
fn test_ant_compile_preserves_build_failed_and_errors() {
    let raw = fixture("ant_compile.txt");
    let filtered = common::filter_ant_build(&raw);

    assert!(
        filtered.contains("BUILD FAILED"),
        "BUILD FAILED must be preserved, got:\n{}",
        filtered
    );
    assert!(
        filtered.contains("error:"),
        "javac error lines must be preserved, got:\n{}",
        filtered
    );
}

#[test]
fn test_ant_compile_strips_target_headers_and_chatter() {
    let raw = fixture("ant_compile.txt");
    let filtered = common::filter_ant_build(&raw);

    assert!(
        !filtered.contains("Buildfile:"),
        "Buildfile: header should be stripped, got:\n{}",
        filtered
    );
    // Target-only lines like "clean:" should be stripped
    assert!(
        !filtered.contains("\nclean:\n") && !filtered.starts_with("clean:"),
        "bare target headers should be stripped"
    );
}
