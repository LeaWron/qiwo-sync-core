# qiwo-sync-core

`qiwo-sync-core` is the Rust shared sync component for Qiwo Rime frontends.

It provides:

- WebDAV sync for selected Rime configuration files
- Rime native user dictionary sync through `sync-user-dict`
- `sync`, `push`, `pull`, `init-frost`, and `sync-user-dict` CLI modes
- a local manifest under `.qiwo-sync/manifest.json`
- local conflict backups before overwriting files
- generated build artifact and binary user database filtering

## Build

```bash
cargo build --release -p qiwo-rime-sync
```

The CLI binary is written to:

```text
target/release/qiwo-rime-sync
```

For platform bundles, build the binary or library with the target triple required
by the frontend and copy it into the platform package.

## CLI

```bash
cargo run -p qiwo-rime-sync -- sync \
  --frontend weasel \
  --rime-user-dir "$APPDATA/Rime" \
  --remote-url "https://dav.example.com/qiwo-rime-sync" \
  --username "name" \
  --password-env QIWO_WEBDAV_PASSWORD \
  # there is no --password: a command line is readable by any process running
  # as the same user, and lands in shell history. Frontends set the variable in
  # the child's environment instead.
  --device-id "windows-main"
```

Initialize `rime-frost` into a Rime user directory:

```bash
cargo run -p qiwo-rime-sync -- init-frost \
  --frontend weasel \
  --rime-user-dir "$APPDATA/Rime" \
  --frost-dir "./rime-frost"
```

Sync Rime native user dictionary snapshots:

```bash
cargo run -p qiwo-rime-sync -- sync-user-dict \
  --frontend weasel \
  --rime-user-dir "$APPDATA/Rime" \
  --remote-url "https://dav.example.com/qiwo-rime-sync" \
  --device-id "windows-main"
```

Before syncing user dictionaries, platform frontends should call Rime's
`sync_user_data()` to export local dictionaries into `sync/<device-id>/`.
After pulling remote snapshots, call `sync_user_data()` again so Rime imports
the downloaded snapshots.

`qiwo-sync-core` keeps `installation.yaml` aligned with the WebDAV `device-id`
by setting:

```yaml
installation_id: "<device-id>"
sync_dir: "<absolute-rime-user-dir>/sync"
```

If the device id changes, existing files from the previous `sync/<old-id>/`
directory are migrated into `sync/<new-id>/`.

## Synced Files

Only the **personal** layer syncs. Schemas, dictionaries, `opencc/` and `lua/`
ship with the installer into Rime's *shared* data directory, are identical on
every device, and are therefore never uploaded — syncing them would push ~44 MB
of redistributable data per device to the user's own WebDAV server.

Included:

- `*.custom.yaml` — Rime's supported customisation entry point
- `custom_phrase.txt`
- `sync/**` — the user dictionary snapshots written by `sync_user_data()`

Excluded:

- everything distributed with the installer: `*.schema.yaml`, `*.dict.yaml`,
  `cn_dicts*/**`, `en_dicts/**`, `opencc/**`, `lua/**`, `symbols.yaml`,
  `default.yaml`, `essay.txt`, `*.gram`
- `build/**`, `*.bin`, `*.userdb/**`
- `.git/**`, `.qiwo-sync/**`
- `*.qiwo-part` — staging files from an interrupted download

> The Android frontend reimplements this list in Kotlin
> (`qiwo/sync/FileSelector.kt`). The two must be changed together, or one end
> uploads files the other refuses.

## init-frost

`init-frost` runs at install time, after the installer has staged the shared
data directory (`--frost-dir`). It does two things:

1. Checks that the shared data directory holds what the bundled schemas need
   (`default.yaml`, `rime_frost.schema.yaml`, `cn_dicts/`, `opencc/`, `lua/`, …)
   and reports anything missing.
2. Moves stale copies of distributed files out of the user directory. Older
   versions copied the whole distribution tree (schemas, dictionaries, lua,
   opencc — about 150 MB) into the user directory; Rime resolves the user
   directory first, so those copies shadow the shared data forever and an
   upgraded machine keeps deploying old schemas. A file counts as a stale copy
   when the distribution ships a file at the same relative path. It is moved,
   never deleted, into `<user dir>/qiwo-shadowed-backup/<timestamp>/` together
   with a `MOVED.txt` manifest, so it can be put back by hand.

User-owned files are never touched, even when the distribution ships a file of
the same name: `custom_phrase.txt` (rime-frost ships a sample), `*.custom.yaml`,
`installation.yaml`, `user.yaml`, `sync/`, `build/` and `*.userdb`. It never
writes to a user config file. `--dry-run` only reports what would move.
