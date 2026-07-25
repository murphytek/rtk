pub const RTK_DATA_DIR: &str = "rtk";
pub const HISTORY_DB: &str = "history.db";
pub const CONFIG_TOML: &str = "config.toml";
// Used only from `TomlFilterRegistry::load()`, which the bin's `--test` build
// cannot see as live (the harness replaces `main`, so the run_cli → …  →
// find_matching_filter chain is not a dead-code root). rustc 1.97 tightened this
// analysis and now denies both under `[lints.rust] warnings = "deny"`.
#[cfg_attr(test, allow(dead_code))]
pub const FILTERS_TOML: &str = "filters.toml";
pub const TRUSTED_FILTERS_JSON: &str = "trusted_filters.json";
pub const DEFAULT_HISTORY_DAYS: i64 = 90;
