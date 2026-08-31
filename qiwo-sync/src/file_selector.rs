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
        let path = normalize_path(relative_path);
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
}

fn normalize_path(path: &str) -> String {
    path.replace('\\', "/").trim_start_matches('/').to_string()
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
}
