//! Relative-path normalisation shared by the file selector and the WebDAV client.
//!
//! Both used to carry a byte-identical private copy of this function.

/// Converts a Rime-relative path to the forward-slash, no-leading-slash form the
/// manifest and the WebDAV URLs are built from.
pub(crate) fn normalize_relative(path: &str) -> String {
    path.replace('\\', "/").trim_start_matches('/').to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalizes_separators_and_leading_slashes() {
        assert_eq!(
            normalize_relative("sync\\android\\a.txt"),
            "sync/android/a.txt"
        );
        assert_eq!(
            normalize_relative("/default.custom.yaml"),
            "default.custom.yaml"
        );
        assert_eq!(normalize_relative("//a//b"), "a//b");
        assert_eq!(normalize_relative("plain.yaml"), "plain.yaml");
    }
}
