//! Cross-frontend contract for the manifest wire format.
//!
//! `ManifestSerializer.kt` in qiwo-android hand-writes the same field names that
//! serde's `rename` attributes produce here, kept in step by a comment saying
//! they are "严格一致" — the third such comment with nothing checking it. If
//! either side renames a field, one frontend silently stops being able to read
//! the other's manifest.
//!
//! `tests/fixtures/manifest_sample.json` is the canonical document; qiwo-android
//! keeps a byte-identical copy and runs the same assertions.

use std::path::Path;

use qiwo_sync::types::SyncManifest;

/// SHA-256 (LF-normalised) of the sample; same constant on the Kotlin side.
const MANIFEST_SAMPLE_SHA256: &str =
    "6dfdf212eec437cd21634ec696fe66c9e638eb15d529ae9755a875b956bb7fcc";

/// Every key the wire format is expected to carry, at both levels.
const MANIFEST_KEYS: &[&str] = &["version", "deviceId", "frontend", "updatedAtUtc", "files"];
const ENTRY_KEYS: &[&str] = &["relativePath", "size", "sha256", "lastWriteUtc"];

fn sample() -> (std::path::PathBuf, String) {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures")
        .join("manifest_sample.json");
    let raw = std::fs::read_to_string(&path)
        .unwrap_or_else(|e| panic!("reading {}: {e}", path.display()));
    (path, raw)
}

#[test]
fn manifest_sample_is_unchanged_on_both_sides() {
    use sha2::{Digest, Sha256};

    let (path, raw) = sample();
    let normalised = raw.replace("\r\n", "\n");
    let digest = format!("{:x}", Sha256::digest(normalised.as_bytes()));

    assert_eq!(
        digest,
        MANIFEST_SAMPLE_SHA256,
        "the shared manifest sample changed.\n\
         Mirror {} to qiwo-android at \
         app/src/test/resources/qiwo/sync/manifest_sample.json and update \
         MANIFEST_SAMPLE_SHA256 in both repositories.",
        path.display()
    );
}

#[test]
fn manifest_sample_deserializes_with_the_expected_values() {
    let (_, raw) = sample();
    let manifest: SyncManifest = serde_json::from_str(&raw).expect("sample parses");

    assert_eq!(manifest.version, 1);
    assert_eq!(manifest.device_id, "windows-main");
    assert_eq!(manifest.frontend, "Weasel");
    assert_eq!(
        manifest.updated_at_utc.to_rfc3339(),
        "2026-08-31T12:34:56+00:00"
    );
    assert_eq!(manifest.files.len(), 2);

    let entry = manifest
        .files
        .get("sync/android-pixel-7/rime_frost.userdb.txt")
        .expect("nested entry is keyed by its relative path");
    assert_eq!(
        entry.relative_path,
        "sync/android-pixel-7/rime_frost.userdb.txt"
    );
    assert_eq!(entry.size, 4096);
    assert_eq!(
        entry.sha256,
        "0011223344556677889900aabbccddeeff00112233445566778899aabbccddee"
    );
    assert_eq!(
        entry.last_write_utc.to_rfc3339(),
        "2026-08-31T09:08:07+00:00"
    );
    assert_eq!(entry.e_tag, None, "eTag is optional and absent here");
}

/// The field *names* are the contract. A serde `rename` changed on this side
/// without the matching edit in `ManifestSerializer.kt` shows up here.
#[test]
fn serialized_manifest_uses_exactly_the_agreed_field_names() {
    let (_, raw) = sample();
    let manifest: SyncManifest = serde_json::from_str(&raw).unwrap();
    let round_tripped: serde_json::Value =
        serde_json::from_str(&serde_json::to_string(&manifest).unwrap()).unwrap();

    let mut top: Vec<&str> = round_tripped
        .as_object()
        .expect("object")
        .keys()
        .map(String::as_str)
        .collect();
    top.sort_unstable();
    let mut expected_top = MANIFEST_KEYS.to_vec();
    expected_top.sort_unstable();
    assert_eq!(top, expected_top, "manifest field names");

    for (path, entry) in round_tripped["files"].as_object().expect("files object") {
        let mut keys: Vec<&str> = entry
            .as_object()
            .expect("entry object")
            .keys()
            .map(String::as_str)
            .collect();
        keys.sort_unstable();
        let mut expected_entry = ENTRY_KEYS.to_vec();
        expected_entry.sort_unstable();
        assert_eq!(keys, expected_entry, "entry field names for {path}");
    }
}

/// `eTag` is written only when present, so an entry that has one must still
/// round-trip — the Kotlin side never writes it and must tolerate its absence.
#[test]
fn etag_is_omitted_when_absent_and_kept_when_present() {
    let (_, raw) = sample();
    let mut manifest: SyncManifest = serde_json::from_str(&raw).unwrap();

    let without = serde_json::to_string(&manifest).unwrap();
    assert!(!without.contains("eTag"), "absent eTag must not be written");

    manifest.files.values_mut().next().unwrap().e_tag = Some("\"abc123\"".to_string());
    let with: serde_json::Value =
        serde_json::from_str(&serde_json::to_string(&manifest).unwrap()).unwrap();
    let has_etag = with["files"]
        .as_object()
        .unwrap()
        .values()
        .any(|e| e.get("eTag").is_some());
    assert!(
        has_etag,
        "present eTag must be written under that exact name"
    );
}
