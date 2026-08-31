/// Selects Rime files that are safe to share through WebDAV.
///
/// Only the *personal* layer syncs. Schemas, dictionaries, opencc data and lua
/// ship with the installer into Rime's shared data directory and are identical
/// on every device, so putting them on the user's WebDAV server would push
/// ~44 MB of redistributable data per device for no benefit — and it
/// contradicts the project rule that the main dictionary is distributed, not
/// synced. What the user actually authors lives in `*.custom.yaml`,
/// `custom_phrase.txt` and the `sync/` snapshots.
///
/// Editing `cn_dicts/` or `opencc/` directly is an advanced move and is the
/// user's own to manage; `*.custom.yaml` is Rime's supported customisation
/// entry point and does sync.
///
/// **This list is mirrored in `FileSelector.kt` in qiwo-android. Change both.**
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct FileSelector;

impl FileSelector {
    const INCLUDED_EXACT: &'static [&'static str] = &["custom_phrase.txt"];
    const INCLUDED_EXTENSIONS: &'static [&'static str] = &[".custom.yaml"];
    const INCLUDED_DIRECTORIES: &'static [&'static str] = &["sync/"];
    const EXCLUDED_DIRECTORIES: &'static [&'static str] = &[".git/", ".qiwo-sync/", "build/"];
    // `.bin` already covers `.table.bin` and `.reverse.bin`; `.qiwo-part` is the
    // staging suffix left behind by an interrupted download.
    const EXCLUDED_EXTENSIONS: &'static [&'static str] = &[".bin", ".qiwo-part"];
    const EXCLUDED_SUFFIXES: &'static [&'static str] = &[".userdb"];

    pub fn should_sync(&self, relative_path: &str) -> bool {
        let path = crate::paths::normalize_relative(relative_path);
        let lower = path.to_lowercase();

        // 排除特定目录
        if Self::EXCLUDED_DIRECTORIES
            .iter()
            .any(|d| lower.starts_with(d))
        {
            return false;
        }

        // 排除路径中包含 .userdb 的目录段
        if lower.split('/').any(|seg| seg.ends_with(".userdb")) {
            return false;
        }

        // 排除后缀和扩展名
        if Self::EXCLUDED_SUFFIXES.iter().any(|s| lower.ends_with(s))
            || Self::EXCLUDED_EXTENSIONS.iter().any(|e| lower.ends_with(e))
        {
            return false;
        }

        // 精确文件名匹配
        let file_name = path.rsplit('/').next().unwrap_or(&path);
        if Self::INCLUDED_EXACT
            .iter()
            .any(|e| e.eq_ignore_ascii_case(file_name))
        {
            return true;
        }

        // 扩展名匹配
        if Self::INCLUDED_EXTENSIONS.iter().any(|e| lower.ends_with(e)) {
            return true;
        }

        // 目录匹配
        if Self::INCLUDED_DIRECTORIES
            .iter()
            .any(|d| lower.starts_with(d))
        {
            return true;
        }

        false
    }

    /// Whether a directory can contain anything [`Self::should_sync`] accepts.
    ///
    /// Used to prune the scan. Derived from the same two lists rather than
    /// hard-coded, so it stays correct if the include set changes: a directory
    /// is worth entering only when it lies on the path to an included
    /// directory, in either direction.
    pub fn should_descend(&self, relative_dir: &str) -> bool {
        let dir = crate::paths::normalize_relative(relative_dir);
        if dir.is_empty() {
            return true;
        }

        let lower = format!("{}/", dir.to_lowercase());

        if Self::EXCLUDED_DIRECTORIES
            .iter()
            .any(|d| lower.starts_with(d))
        {
            return false;
        }

        if lower.split('/').any(|seg| seg.ends_with(".userdb")) {
            return false;
        }

        Self::INCLUDED_DIRECTORIES
            .iter()
            .any(|d| lower.starts_with(d) || d.starts_with(&lower))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_should_sync_custom_yaml() {
        let fs = FileSelector;
        assert!(fs.should_sync("default.custom.yaml"));
        assert!(fs.should_sync("weasel.custom.yaml"));
    }

    /// Distributed data ships with the installer into Rime's shared data
    /// directory; syncing it would push ~44 MB per device to the user's WebDAV
    /// server and duplicate what every device already has.
    #[test]
    fn test_exclude_distributed_schema_and_dictionary_data() {
        let fs = FileSelector;
        assert!(!fs.should_sync("rime_frost.schema.yaml"));
        assert!(!fs.should_sync("rime_frost.dict.yaml"));
        assert!(!fs.should_sync("cn_dicts/base.dict.yaml"));
        assert!(!fs.should_sync("cn_dicts_cell/composite.dict.yaml"));
        assert!(!fs.should_sync("opencc/s2t.json"));
        assert!(!fs.should_sync("opencc/emoji.json"));
        assert!(!fs.should_sync("lua/corrector.lua"));
        assert!(!fs.should_sync("symbols.yaml"));
        assert!(!fs.should_sync("essay.txt"));
        assert!(!fs.should_sync("default.yaml"));
    }

    /// Rime's supported customisation entry point still syncs, so a user who
    /// patches a schema or a dictionary through `*.custom.yaml` keeps it.
    #[test]
    fn test_should_sync_exact_files() {
        let fs = FileSelector;
        assert!(fs.should_sync("custom_phrase.txt"));
        assert!(fs.should_sync("rime_frost.custom.yaml"));
        assert!(fs.should_sync("symbols.custom.yaml"));
    }

    #[test]
    fn test_should_sync_sync_dir() {
        let fs = FileSelector;
        // Rime sync_user_data() exports .userdb.txt files under sync/<device>/
        assert!(fs.should_sync("sync/my-device/rime_frost.userdb.txt"));
    }

    #[test]
    fn test_exclude_build() {
        let fs = FileSelector;
        assert!(!fs.should_sync("build/rime_frost.schema.yaml"));
    }

    #[test]
    fn test_exclude_bin() {
        let fs = FileSelector;
        assert!(!fs.should_sync("rime_frost.table.bin"));
        assert!(!fs.should_sync("rime_frost.reverse.bin"));
    }

    #[test]
    fn test_exclude_interrupted_download_staging_files() {
        let fs = FileSelector;
        assert!(!fs.should_sync(".default.custom.yaml.qiwo-part"));
        assert!(!fs.should_sync("sync/my-device/.rime_frost.userdb.txt.qiwo-part"));
    }

    #[test]
    fn test_exclude_userdb_dir() {
        let fs = FileSelector;
        assert!(!fs.should_sync("rime_frost.userdb/0001.sqlite3"));
    }

    #[test]
    fn test_exclude_qiwo_sync_state() {
        let fs = FileSelector;
        assert!(!fs.should_sync(".qiwo-sync/manifest.json"));
    }

    #[test]
    fn descends_only_into_directories_that_can_hold_synced_files() {
        let fs = FileSelector;
        // The scan root, and the one included directory tree.
        assert!(fs.should_descend(""));
        assert!(fs.should_descend("sync"));
        assert!(fs.should_descend("sync/windows-main"));
        assert!(fs.should_descend("sync\\android"));

        // Distributed data: only ever holds files should_sync() rejects.
        assert!(!fs.should_descend("cn_dicts"));
        assert!(!fs.should_descend("cn_dicts_cell"));
        assert!(!fs.should_descend("opencc"));
        assert!(!fs.should_descend("lua"));
        assert!(!fs.should_descend("lua/aux_code"));

        // Explicitly excluded, plus the LevelDB stores.
        assert!(!fs.should_descend("build"));
        assert!(!fs.should_descend(".git"));
        assert!(!fs.should_descend(".qiwo-sync"));
        assert!(!fs.should_descend(".qiwo-sync/backups"));
        assert!(!fs.should_descend("rime_frost.userdb"));
        assert!(!fs.should_descend("sync/android/rime_frost.userdb"));
    }

    /// Pruning must never skip a directory that holds a file we would sync.
    #[test]
    fn pruning_never_hides_a_syncable_file() {
        let fs = FileSelector;
        for path in [
            "sync/windows-main/rime_frost.userdb.txt",
            "sync/android/melt_eng.userdb.txt",
            "default.custom.yaml",
            "custom_phrase.txt",
        ] {
            assert!(fs.should_sync(path), "fixture {path} should sync");

            // Every ancestor directory must be reachable.
            let segments: Vec<&str> = path.split('/').collect();
            for depth in 0..segments.len() - 1 {
                let dir = segments[..=depth].join("/");
                assert!(
                    fs.should_descend(&dir),
                    "pruning at {dir} would hide {path}"
                );
            }
        }
    }
}
