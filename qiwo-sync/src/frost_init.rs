use std::path::Path;

use anyhow::Result;

use crate::types::{SyncMode, SyncRequest, SyncSummary};

/// Files and directories the shared data directory must hold for the bundled
/// schemas to deploy. Anything missing here means the installer staged the data
/// wrongly, which otherwise only shows up as a confusing Rime deployment error.
const REQUIRED_ENTRIES: &[&str] = &[
    "default.yaml",
    "rime_frost.schema.yaml",
    "rime_frost.dict.yaml",
    "cn_dicts",
    "cn_dicts_cell",
    "opencc",
    "lua",
];

pub struct FrostInitializer;

impl FrostInitializer {
    /// Checks that the shared data directory looks deployable.
    ///
    /// This used to *write* to the user's directory: it spliced Qiwo's default
    /// switcher hotkey, the `auto_commit_spacing` save option and the matching
    /// per-schema switch into `default.custom.yaml` and every
    /// `rime_frost*.custom.yaml`, by locating the `patch:` line and inserting
    /// text after it.
    ///
    /// That was destructive on hand-edited configs. A user who had written
    /// `patch: # 我的配置` — a comment after the key, which is ordinary in Rime
    /// configs — did not match the anchor, so a *second* top-level `patch:` key
    /// was appended; yaml-cpp keeps the last one and the user's entire
    /// `schema_list` was silently discarded. `patch :` behaved the same way. A
    /// setting that appeared inside a comment counted as already present, so it
    /// was never applied.
    ///
    /// Those settings now ship in qiwo-rime-data's `default.yaml` and schema
    /// files, which is Rime's own layering: the distributed layer supplies
    /// defaults, the user overrides them in their own `*.custom.yaml`, and we
    /// never write to files we do not own.
    pub async fn initialize(request: &SyncRequest) -> Result<SyncSummary> {
        let shared_data_dir = request
            .frost_dir
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("FrostDir is required for init-frost."))?;

        if !shared_data_dir.exists() {
            anyhow::bail!(
                "shared data directory does not exist: {}",
                shared_data_dir.display()
            );
        }

        let missing = missing_entries(shared_data_dir);
        let mut summary =
            SyncSummary::new(SyncMode::InitFrost, request.frontend, &request.device_id);

        if missing.is_empty() {
            summary.messages.push(format!(
                "Shared Rime data looks complete: {}",
                shared_data_dir.display()
            ));
        } else {
            summary.skipped = missing.len() as u32;
            summary.messages.push(format!(
                "warning: {} is missing {} — the installer may not have staged the shared data \
                 correctly.",
                shared_data_dir.display(),
                missing.join(", ")
            ));
        }

        Ok(summary)
    }
}

fn missing_entries(shared_data_dir: &Path) -> Vec<&'static str> {
    REQUIRED_ENTRIES
        .iter()
        .filter(|entry| !shared_data_dir.join(entry).exists())
        .copied()
        .collect()
}

#[cfg(test)]
mod tests {
    use std::fs as std_fs;
    use std::time::{SystemTime, UNIX_EPOCH};

    use super::*;
    use crate::types::{Frontend, SyncMode, SyncRequest};

    fn runtime() -> tokio::runtime::Runtime {
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap()
    }

    fn temp_dir(name: &str) -> std::path::PathBuf {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        std::env::temp_dir().join(format!("qiwo-sync-core-{name}-{nanos}"))
    }

    fn stage_complete_shared_dir(dir: &Path) {
        for entry in REQUIRED_ENTRIES {
            let path = dir.join(entry);
            if entry.ends_with(".yaml") {
                std_fs::create_dir_all(path.parent().unwrap()).unwrap();
                std_fs::write(path, "# staged\n").unwrap();
            } else {
                std_fs::create_dir_all(path).unwrap();
            }
        }
    }

    fn run(shared: &Path, user: &Path) -> SyncSummary {
        let request = SyncRequest {
            frontend: Frontend::IbusRime,
            rime_user_dir: user.to_path_buf(),
            remote_url: None,
            username: None,
            password: None,
            device_id: "test".into(),
            mode: SyncMode::InitFrost,
            frost_dir: Some(shared.to_path_buf()),
            dry_run: false,
        };
        runtime()
            .block_on(FrostInitializer::initialize(&request))
            .unwrap()
    }

    /// The whole point of the rewrite: the user's directory is never written to.
    /// Splicing into a hand-edited `default.custom.yaml` used to be able to
    /// discard the user's `schema_list` entirely.
    #[test]
    fn init_frost_never_writes_to_the_user_directory() {
        let shared = temp_dir("shared-readonly");
        let user = temp_dir("user-readonly");
        std_fs::create_dir_all(&user).unwrap();
        stage_complete_shared_dir(&shared);

        // A hand-edited config of the exact shape the old splicing destroyed.
        let hand_edited = "patch: # 我的配置\n  schema_list:\n    - schema: luna_pinyin\n";
        std_fs::write(user.join("default.custom.yaml"), hand_edited).unwrap();

        let summary = run(&shared, &user);
        assert!(
            summary.messages.iter().any(|m| m.contains("complete")),
            "{:?}",
            summary.messages
        );

        assert_eq!(
            std_fs::read_to_string(user.join("default.custom.yaml")).unwrap(),
            hand_edited,
            "the user's file must be byte-for-byte untouched"
        );
        let entries: Vec<String> = std_fs::read_dir(&user)
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        assert_eq!(
            entries,
            vec!["default.custom.yaml".to_string()],
            "no files may be created either, got {entries:?}"
        );

        let _ = std_fs::remove_dir_all(shared);
        let _ = std_fs::remove_dir_all(user);
    }

    #[test]
    fn init_frost_reports_a_half_staged_shared_directory() {
        let shared = temp_dir("shared-partial");
        let user = temp_dir("user-partial");
        std_fs::create_dir_all(&user).unwrap();
        stage_complete_shared_dir(&shared);
        // Exactly the failure the old flat-vs-nested staging bug produced.
        std_fs::remove_dir_all(shared.join("cn_dicts")).unwrap();
        std_fs::remove_dir_all(shared.join("opencc")).unwrap();

        let summary = run(&shared, &user);

        assert_eq!(summary.skipped, 2);
        let message = summary.messages.join("\n");
        assert!(message.contains("cn_dicts"), "{message}");
        assert!(message.contains("opencc"), "{message}");

        let _ = std_fs::remove_dir_all(shared);
        let _ = std_fs::remove_dir_all(user);
    }

    #[test]
    fn init_frost_fails_when_the_shared_directory_is_absent() {
        let shared = temp_dir("shared-missing");
        let user = temp_dir("user-missing");
        std_fs::create_dir_all(&user).unwrap();

        let request = SyncRequest {
            frontend: Frontend::IbusRime,
            rime_user_dir: user.clone(),
            remote_url: None,
            username: None,
            password: None,
            device_id: "test".into(),
            mode: SyncMode::InitFrost,
            frost_dir: Some(shared),
            dry_run: false,
        };
        let error = runtime()
            .block_on(FrostInitializer::initialize(&request))
            .expect_err("a missing shared data directory is fatal");
        assert!(format!("{error:#}").contains("does not exist"));

        let _ = std_fs::remove_dir_all(user);
    }
}
