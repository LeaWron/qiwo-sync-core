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

/// SHA-256 of the case file with line endings normalised to LF.
///
/// The same constant is asserted by `FileSelectorContractTest` in qiwo-android.
/// Editing the cases on one side only makes the *other* repository's build fail
/// on its next run, which is the point: the two copies must move together.
const CASES_SHA256: &str = "bbaaa8a30b0a1d1d9844b8c3b29003ddbef0708c58203f2cfe45eaa7cf757da6";

fn sha256_lf(bytes: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    let normalised: Vec<u8> = String::from_utf8_lossy(bytes).replace("\r\n", "\n").into();
    format!("{:x}", Sha256::digest(&normalised))
}

#[test]
fn shared_contract_file_is_unchanged_on_both_sides() {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures")
        .join("file_selector_cases.jsonl");
    let raw = std::fs::read(&path).unwrap_or_else(|e| panic!("reading {}: {e}", path.display()));

    assert_eq!(
        sha256_lf(&raw),
        CASES_SHA256,
        "the shared contract cases changed.\n\
         Mirror {} to qiwo-android at \
         app/src/test/resources/qiwo/sync/file_selector_cases.jsonl and update \
         CASES_SHA256 in both repositories.",
        path.display()
    );
}

/// SHA-256 (LF-normalised) of the device-id case file; same constant on the
/// Kotlin side. See [`CASES_SHA256`].
const DEVICE_ID_CASES_SHA256: &str =
    "5471a6d4bb96916d62d21cabb644ff965a61afa883e82bd0621d50b13b15e562";

fn fixture(name: &str) -> (std::path::PathBuf, String) {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures")
        .join(name);
    let raw = std::fs::read_to_string(&path)
        .unwrap_or_else(|e| panic!("reading {}: {e}", path.display()));
    (path, raw)
}

/// The device id becomes a `sync/<id>/` directory name, a WebDAV path segment,
/// and the value of `installation_id` in `installation.yaml`. The two frontends
/// must derive it identically or the same configured name produces two
/// different snapshot directories.
#[test]
fn safe_device_id_matches_the_shared_contract() {
    let (path, raw) = fixture("device_id_cases.jsonl");

    let mut checked = 0usize;
    let mut failures = Vec::new();
    for (index, line) in raw.lines().enumerate() {
        if line.trim().is_empty() {
            continue;
        }
        let case: serde_json::Value = serde_json::from_str(line)
            .unwrap_or_else(|e| panic!("{}:{}: {e}", path.display(), index + 1));
        let input = case["input"].as_str().expect("input");
        let expected = case["expected"].as_str().expect("expected");

        checked += 1;
        let actual = qiwo_sync::installation::safe_device_id(input);
        if actual != expected {
            failures.push(format!(
                "line {}: {input:?} -> {actual:?}, expected {expected:?} ({})",
                index + 1,
                case["why"].as_str().unwrap_or("")
            ));
        }
    }

    assert!(checked > 0, "fixture file is empty");
    assert!(
        failures.is_empty(),
        "{} of {checked} device-id case(s) failed:\n{}",
        failures.len(),
        failures.join("\n")
    );
}

/// A sanitised id must be safe in all three places it lands.
#[test]
fn safe_device_id_output_is_always_path_and_yaml_safe() {
    let (_, raw) = fixture("device_id_cases.jsonl");

    for line in raw.lines().filter(|l| !l.trim().is_empty()) {
        let case: serde_json::Value = serde_json::from_str(line).unwrap();
        let out = qiwo_sync::installation::safe_device_id(case["input"].as_str().unwrap());

        assert!(!out.is_empty(), "never empty");
        assert!(
            out.chars().all(|c| c.is_ascii_lowercase()
                || c.is_ascii_digit()
                || matches!(c, '.' | '_' | '-')),
            "{out:?} contains a character that is unsafe as a path segment or inside a \
             double-quoted YAML scalar"
        );
    }
}

#[test]
fn device_id_contract_file_is_unchanged_on_both_sides() {
    let (path, _) = fixture("device_id_cases.jsonl");
    let raw = std::fs::read(&path).unwrap();

    assert_eq!(
        sha256_lf(&raw),
        DEVICE_ID_CASES_SHA256,
        "the shared device-id cases changed.\n\
         Mirror {} to qiwo-android at \
         app/src/test/resources/qiwo/sync/device_id_cases.jsonl and update \
         DEVICE_ID_CASES_SHA256 in both repositories.",
        path.display()
    );
}

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
