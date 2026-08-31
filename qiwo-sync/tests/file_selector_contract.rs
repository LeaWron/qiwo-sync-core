//! Cross-frontend contract for which files sync.
//!
//! `tests/fixtures/file_selector_cases.jsonl` is the canonical case list. The
//! Android frontend keeps a byte-identical copy at
//! `app/src/test/resources/qiwo/sync/file_selector_cases.jsonl` in qiwo-android
//! and runs it against its Kotlin `FileSelector`.
//!
//! Before this existed the two implementations were kept in step by a comment
//! that said "严格一致" — nothing checked it, so a change on one side would have
//! silently made one frontend upload files the other refuses.

use std::path::Path;

use qiwo_sync::file_selector::FileSelector;

#[test]
fn file_selector_matches_the_shared_contract() {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures")
        .join("file_selector_cases.jsonl");
    let raw = std::fs::read_to_string(&path)
        .unwrap_or_else(|e| panic!("reading {}: {e}", path.display()));

    let selector = FileSelector;
    let mut checked = 0usize;
    let mut failures = Vec::new();

    for (index, line) in raw.lines().enumerate() {
        if line.trim().is_empty() {
            continue;
        }

        let case: serde_json::Value = serde_json::from_str(line)
            .unwrap_or_else(|e| panic!("{}:{}: {e}", path.display(), index + 1));
        let case_path = case["path"].as_str().expect("path");
        let expected = case["sync"].as_bool().expect("sync");

        checked += 1;
        let actual = selector.should_sync(case_path);
        if actual != expected {
            failures.push(format!(
                "line {}: {case_path:?} -> {actual}, expected {expected} ({})",
                index + 1,
                case["why"].as_str().unwrap_or("")
            ));
        }
    }

    assert!(checked > 0, "fixture file is empty");
    assert!(
        failures.is_empty(),
        "{} of {checked} contract case(s) failed:\n{}",
        failures.len(),
        failures.join("\n")
    );
}
