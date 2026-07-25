//! Integration tests for JVM filter functions against realistic captured fixtures.
//!
//! Each test:
//! - Reads a fixture file from tests/fixtures/jvm/
//! - Runs it through the appropriate filter function (via tests/common/mod.rs)
//! - Asserts filtered output is at least 40% smaller than raw input (a deliberately
//!   conservative floor — fixtures regularly hit 70-90% in practice; the threshold
//!   only fails when a regression cuts compression in half)
//! - Asserts critical signal lines are preserved (errors, BUILD result)

// NOTE: the Maven sections were removed when the fork adopted upstream's Rust
// `mvn` module (rtk-ai/rtk 6c4950e), which replaced murphytek's mvn-build.toml
// filter and ships its own fixtures and tests. Gradle and Ant remain fork-only.
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
// 1. gradle_test_with_spotbugs_flood.txt → filter_gradle_test
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
// 2. ant_compile.txt → filter_ant_build
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
