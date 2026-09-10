use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use walkdir::WalkDir;

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

/// Where stale copies of distributed files are moved to, inside the user
/// directory. Rime never reads from it and the sync selector's whitelist does
/// not descend into it, so nothing in there can shadow or sync anything.
pub const SHADOWED_BACKUP_DIR: &str = "qiwo-shadowed-backup";

/// Manifest written next to the moved files: one relative path per line, so a
/// user can put everything back by hand if they ever need to.
const MOVED_MANIFEST: &str = "MOVED.txt";

/// Files that belong to the user even when the distribution ships a file of the
/// same name. `custom_phrase.txt` is the important one: rime-frost ships a
/// sample, and the user's own phrases live in a file of exactly that name.
const USER_OWNED_FILES: &[&str] = &["custom_phrase.txt", "installation.yaml", "user.yaml"];

/// Top-level directories that are never distribution copies.
const USER_OWNED_DIRECTORIES: &[&str] = &["sync", "build", SHADOWED_BACKUP_DIR];

pub struct FrostInitializer;

impl FrostInitializer {
    /// Checks that the shared data directory looks deployable, then moves stale
    /// copies of distributed files out of the user directory.
    ///
    /// This used to *write* to the user's config files: it spliced Qiwo's
    /// default switcher hotkey, the `auto_commit_spacing` save option and the
    /// matching per-schema switch into `default.custom.yaml` and every
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
    ///
    /// The one thing we still touch in the user directory is our own leftovers.
    /// Before the shared data directory was staged flat, init-frost copied the
    /// whole distribution tree (schemas, dictionaries, lua, opencc, ~150 MB) into
    /// the user directory. Rime resolves the user directory first, so those
    /// copies shadow the shared data forever: a machine upgraded from such a
    /// version keeps deploying the old schemas no matter how many updates it
    /// installs. Such a copy is recognised by being a file the distribution
    /// ships at the same relative path, and it is moved (never deleted) into
    /// [`SHADOWED_BACKUP_DIR`]. User-owned files are left alone even when the
    /// distribution ships a file of the same name.
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

        let user_dir = &request.rime_user_dir;
        if is_same_directory(shared_data_dir, user_dir) {
            summary.messages.push(
                "User directory is the shared data directory; nothing can shadow it.".to_string(),
            );
            return Ok(summary);
        }

        let shadowing = find_shadowing_copies(shared_data_dir, user_dir)?;
        if shadowing.is_empty() {
            return Ok(summary);
        }

        if request.dry_run {
            summary.messages.push(format!(
                "dry run: {} stale copies of distributed files shadow the shared data directory \
                 and would be moved into {}: {}",
                shadowing.len(),
                user_dir.join(SHADOWED_BACKUP_DIR).display(),
                describe(&shadowing)
            ));
            return Ok(summary);
        }

        let backup_dir = move_shadowing_copies_aside(user_dir, &shadowing)?;
        summary.messages.push(format!(
            "Moved {} stale copies of distributed files out of the user directory; they were \
             shadowing the shared data directory, so Rime kept deploying outdated schemas and \
             dictionaries. Backup (with {MOVED_MANIFEST}): {}",
            shadowing.len(),
            backup_dir.display()
        ));

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

fn is_same_directory(a: &Path, b: &Path) -> bool {
    match (std::fs::canonicalize(a), std::fs::canonicalize(b)) {
        (Ok(a), Ok(b)) => a == b,
        _ => a == b,
    }
}

/// Relative paths (as shipped in the shared data directory) that also exist as
/// regular files in the user directory and are not user-owned. Sorted, so the
/// manifest and the messages are stable.
fn find_shadowing_copies(shared_data_dir: &Path, user_dir: &Path) -> Result<Vec<PathBuf>> {
    let mut shadowing = Vec::new();
    for entry in WalkDir::new(shared_data_dir).follow_links(false) {
        let entry = entry.with_context(|| {
            format!(
                "failed to walk the shared data directory {}",
                shared_data_dir.display()
            )
        })?;
        if !entry.file_type().is_file() {
            continue;
        }
        let relative = match entry.path().strip_prefix(shared_data_dir) {
            Ok(relative) => relative,
            Err(_) => continue,
        };
        if is_user_owned(relative) {
            continue;
        }
        if user_dir.join(relative).is_file() {
            shadowing.push(relative.to_path_buf());
        }
    }
    shadowing.sort();
    Ok(shadowing)
}

/// Whether a path, relative to the user directory, belongs to the user rather
/// than to the distribution — regardless of what the distribution ships.
fn is_user_owned(relative: &Path) -> bool {
    let normalized = relative.to_string_lossy().replace('\\', "/");
    let lower = normalized.to_lowercase();

    if USER_OWNED_FILES.contains(&lower.as_str()) {
        return true;
    }
    if lower.ends_with(".custom.yaml") {
        return true;
    }
    let mut segments = lower.split('/');
    let first = segments.next().unwrap_or_default();
    if USER_OWNED_DIRECTORIES.contains(&first) && normalized.contains('/') {
        return true;
    }
    lower
        .split('/')
        .any(|segment| segment.ends_with(".userdb") || segment.ends_with(".userdb.txt"))
}

fn describe(paths: &[PathBuf]) -> String {
    const SHOWN: usize = 8;
    let mut names: Vec<String> = paths
        .iter()
        .take(SHOWN)
        .map(|p| p.to_string_lossy().replace('\\', "/"))
        .collect();
    if paths.len() > SHOWN {
        names.push(format!("… and {} more", paths.len() - SHOWN));
    }
    names.join(", ")
}

/// Moves the given user-directory files into a fresh, timestamped directory
/// under [`SHADOWED_BACKUP_DIR`], keeping their relative layout, and writes the
/// manifest. Directories left empty by the move are removed; anything else in
/// them stays where it is.
fn move_shadowing_copies_aside(user_dir: &Path, relative_paths: &[PathBuf]) -> Result<PathBuf> {
    let backup_dir = fresh_backup_dir(user_dir);
    let mut moved: Vec<String> = Vec::with_capacity(relative_paths.len());
    let mut failures: Vec<String> = Vec::new();

    for relative in relative_paths {
        let source = user_dir.join(relative);
        let target = backup_dir.join(relative);
        let outcome = target
            .parent()
            .map(std::fs::create_dir_all)
            .transpose()
            .and_then(|_| std::fs::rename(&source, &target));
        match outcome {
            Ok(()) => moved.push(relative.to_string_lossy().replace('\\', "/")),
            Err(error) => failures.push(format!("{}: {error}", source.display())),
        }
    }

    if !moved.is_empty() {
        let mut manifest = moved.join("\n");
        manifest.push('\n');
        std::fs::write(backup_dir.join(MOVED_MANIFEST), manifest)
            .with_context(|| format!("failed to write {MOVED_MANIFEST}"))?;
        prune_empty_parents(user_dir, relative_paths);
    }

    if !failures.is_empty() {
        anyhow::bail!(
            "moved {} of {} shadowing copies into {}, but {} could not be moved: {}",
            moved.len(),
            relative_paths.len(),
            backup_dir.display(),
            failures.len(),
            failures.join("; ")
        );
    }
    Ok(backup_dir)
}

fn fresh_backup_dir(user_dir: &Path) -> PathBuf {
    let stamp = chrono::Local::now().format("%Y%m%d-%H%M%S").to_string();
    let root = user_dir.join(SHADOWED_BACKUP_DIR);
    let mut candidate = root.join(&stamp);
    let mut suffix = 2;
    while candidate.exists() {
        candidate = root.join(format!("{stamp}-{suffix}"));
        suffix += 1;
    }
    candidate
}

/// Removes directories that the move emptied, walking up towards (but never
/// touching) the user directory itself. `remove_dir` refuses non-empty
/// directories, which is exactly the check we want.
fn prune_empty_parents(user_dir: &Path, relative_paths: &[PathBuf]) {
    let mut parents: Vec<PathBuf> = relative_paths
        .iter()
        .filter_map(|p| p.parent().map(Path::to_path_buf))
        .filter(|p| !p.as_os_str().is_empty())
        .collect();
    parents.sort();
    parents.dedup();
    // Deepest first, so a directory is only tried after its children.
    parents.sort_by_key(|p| std::cmp::Reverse(p.components().count()));
    for parent in parents {
        let mut current = Some(parent.as_path());
        while let Some(dir) = current {
            if dir.as_os_str().is_empty() || std::fs::remove_dir(user_dir.join(dir)).is_err() {
                break;
            }
            current = dir.parent();
        }
    }
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

    fn write(path: &Path, content: &str) {
        std_fs::create_dir_all(path.parent().unwrap()).unwrap();
        std_fs::write(path, content).unwrap();
    }

    fn read(path: &Path) -> String {
        std_fs::read_to_string(path).unwrap()
    }

    fn request(shared: &Path, user: &Path, dry_run: bool) -> SyncRequest {
        SyncRequest {
            frontend: Frontend::IbusRime,
            rime_user_dir: user.to_path_buf(),
            remote_url: None,
            username: None,
            password: None,
            device_id: "test".into(),
            mode: SyncMode::InitFrost,
            frost_dir: Some(shared.to_path_buf()),
            dry_run,
        }
    }

    fn run(shared: &Path, user: &Path) -> SyncSummary {
        runtime()
            .block_on(FrostInitializer::initialize(&request(shared, user, false)))
            .unwrap()
    }

    fn backup_dirs(user: &Path) -> Vec<PathBuf> {
        let root = user.join(SHADOWED_BACKUP_DIR);
        if !root.exists() {
            return Vec::new();
        }
        let mut dirs: Vec<PathBuf> = std_fs::read_dir(root)
            .unwrap()
            .map(|e| e.unwrap().path())
            .collect();
        dirs.sort();
        dirs
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

        let error = runtime()
            .block_on(FrostInitializer::initialize(&request(
                &shared, &user, false,
            )))
            .expect_err("a missing shared data directory is fatal");
        assert!(format!("{error:#}").contains("does not exist"));

        let _ = std_fs::remove_dir_all(user);
    }

    /// The layout an old init-frost left behind: the distribution tree copied
    /// into the user directory, next to the user's own files.
    fn stage_shadowed_user_dir(shared: &Path, user: &Path) {
        stage_complete_shared_dir(shared);
        write(&shared.join("lua/radical_fallback.lua"), "-- new\n");
        write(&shared.join("opencc/emoji.json"), "{}\n");
        write(
            &shared.join("custom_phrase.txt"),
            "# sample shipped with rime-frost\n",
        );
        write(&shared.join("weasel.yaml"), "config_version: 1\n");

        write(&user.join("rime_frost.schema.yaml"), "# stale copy\n");
        write(&user.join("default.yaml"), "# stale copy\n");
        write(&user.join("lua/radical_fallback.lua"), "-- stale\n");
        write(&user.join("opencc/emoji.json"), "{ stale }\n");
        // The user's own things, some with the same name as a shipped file.
        write(&user.join("lua/mine.lua"), "-- the user's own script\n");
        write(&user.join("custom_phrase.txt"), "my phrases\n");
        write(&user.join("default.custom.yaml"), "patch: # 我的配置\n");
        write(&user.join("weasel.custom.yaml"), "patch: {}\n");
        write(&user.join("installation.yaml"), "installation_id: x\n");
        write(&user.join("sync/dev/user.yaml"), "var: {}\n");
        write(&user.join("build/rime_frost.schema.yaml"), "# compiled\n");
        write(&user.join("rime_frost.userdb/LOCK"), "");
    }

    #[test]
    fn init_frost_moves_stale_copies_of_distributed_files_aside() {
        let shared = temp_dir("shared-shadow");
        let user = temp_dir("user-shadow");
        stage_shadowed_user_dir(&shared, &user);

        let summary = run(&shared, &user);

        let message = summary.messages.join("\n");
        assert!(message.contains("Moved 4 stale copies"), "{message}");

        // The copies are gone from where Rime would read them...
        for stale in [
            "rime_frost.schema.yaml",
            "default.yaml",
            "lua/radical_fallback.lua",
            "opencc/emoji.json",
        ] {
            assert!(!user.join(stale).exists(), "{stale} must have been moved");
        }
        // ...and sit, unchanged, in exactly one backup directory with a manifest.
        let backups = backup_dirs(&user);
        assert_eq!(backups.len(), 1, "{backups:?}");
        let backup = &backups[0];
        assert_eq!(
            read(&backup.join("rime_frost.schema.yaml")),
            "# stale copy\n"
        );
        assert_eq!(read(&backup.join("lua/radical_fallback.lua")), "-- stale\n");
        assert_eq!(read(&backup.join("opencc/emoji.json")), "{ stale }\n");
        assert_eq!(
            read(&backup.join(MOVED_MANIFEST)),
            "default.yaml\nlua/radical_fallback.lua\nopencc/emoji.json\nrime_frost.schema.yaml\n"
        );

        // Everything the user owns stays put, byte for byte.
        assert_eq!(
            read(&user.join("lua/mine.lua")),
            "-- the user's own script\n"
        );
        assert_eq!(read(&user.join("custom_phrase.txt")), "my phrases\n");
        assert_eq!(
            read(&user.join("default.custom.yaml")),
            "patch: # 我的配置\n"
        );
        assert_eq!(read(&user.join("weasel.custom.yaml")), "patch: {}\n");
        assert_eq!(
            read(&user.join("installation.yaml")),
            "installation_id: x\n"
        );
        assert_eq!(read(&user.join("sync/dev/user.yaml")), "var: {}\n");
        assert_eq!(
            read(&user.join("build/rime_frost.schema.yaml")),
            "# compiled\n"
        );
        assert!(user.join("rime_frost.userdb/LOCK").exists());
        // A directory emptied by the move is removed; one with user files is not.
        assert!(!user.join("opencc").exists());
        assert!(user.join("lua").is_dir());

        let _ = std_fs::remove_dir_all(shared);
        let _ = std_fs::remove_dir_all(user);
    }

    #[test]
    fn init_frost_is_idempotent_once_the_copies_are_gone() {
        let shared = temp_dir("shared-idempotent");
        let user = temp_dir("user-idempotent");
        stage_shadowed_user_dir(&shared, &user);

        run(&shared, &user);
        let second = run(&shared, &user);

        assert!(
            !second.messages.iter().any(|m| m.contains("Moved")),
            "{:?}",
            second.messages
        );
        assert_eq!(backup_dirs(&user).len(), 1, "no second backup directory");

        let _ = std_fs::remove_dir_all(shared);
        let _ = std_fs::remove_dir_all(user);
    }

    #[test]
    fn init_frost_dry_run_only_reports_the_shadowing_copies() {
        let shared = temp_dir("shared-dryrun");
        let user = temp_dir("user-dryrun");
        stage_shadowed_user_dir(&shared, &user);

        let summary = runtime()
            .block_on(FrostInitializer::initialize(&request(&shared, &user, true)))
            .unwrap();

        let message = summary.messages.join("\n");
        assert!(message.contains("dry run: 4 stale copies"), "{message}");
        assert!(message.contains("lua/radical_fallback.lua"), "{message}");
        assert_eq!(read(&user.join("rime_frost.schema.yaml")), "# stale copy\n");
        assert!(backup_dirs(&user).is_empty());

        let _ = std_fs::remove_dir_all(shared);
        let _ = std_fs::remove_dir_all(user);
    }

    /// Test harnesses (and a misconfigured frontend) may point both directories
    /// at the same place; every file would count as its own shadow.
    #[test]
    fn init_frost_leaves_a_user_directory_that_is_the_shared_directory_alone() {
        let shared = temp_dir("shared-is-user");
        stage_complete_shared_dir(&shared);

        let summary = run(&shared, &shared);

        assert!(
            summary
                .messages
                .iter()
                .any(|m| m.contains("nothing can shadow")),
            "{:?}",
            summary.messages
        );
        assert_eq!(read(&shared.join("rime_frost.schema.yaml")), "# staged\n");
        assert!(backup_dirs(&shared).is_empty());

        let _ = std_fs::remove_dir_all(shared);
    }
}
