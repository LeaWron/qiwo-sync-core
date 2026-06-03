/// Selects Rime files that are safe to share through WebDAV.
pub struct FileSelector;

impl FileSelector {
    const INCLUDED_EXACT: &'static [&'static str] = &["custom_phrase.txt", "symbols.yaml"];
    const INCLUDED_EXTENSIONS: &'static [&'static str] =
        &[".custom.yaml", ".schema.yaml", ".dict.yaml"];
    const INCLUDED_DIRECTORIES: &'static [&'static str] = &["opencc/", "lua/", "sync/"];
    const EXCLUDED_DIRECTORIES: &'static [&'static str] = &[".git/", ".qiwo-sync/", "build/"];
    const EXCLUDED_EXTENSIONS: &'static [&'static str] = &[".bin"];
    const EXCLUDED_SUFFIXES: &'static [&'static str] = &[".table.bin", ".reverse.bin", ".userdb"];

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

    #[test]
    fn test_should_sync_schema_dict() {
        let fs = FileSelector;
        assert!(fs.should_sync("rime_frost.schema.yaml"));
        assert!(fs.should_sync("rime_frost.dict.yaml"));
    }

    #[test]
    fn test_should_sync_exact_files() {
        let fs = FileSelector;
        assert!(fs.should_sync("custom_phrase.txt"));
        assert!(fs.should_sync("symbols.yaml"));
    }

    #[test]
    fn test_should_sync_directories() {
        let fs = FileSelector;
        assert!(fs.should_sync("opencc/s2t.json"));
        assert!(fs.should_sync("lua/selector.lua"));
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
