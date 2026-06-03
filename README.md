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
sync_dir: "sync"
```

If the device id changes, existing files from the previous `sync/<old-id>/`
directory are migrated into `sync/<new-id>/`.

## Synced Files

Included:

- `*.custom.yaml`
- `*.schema.yaml`
- `*.dict.yaml`
- `custom_phrase.txt`
- `symbols.yaml`
- `opencc/**`
- `lua/**`
- `sync/**`

Excluded:

- `build/**`
- `*.bin`
- `*.table.bin`
- `*.reverse.bin`
- `*.userdb/**`
- `.git/**`
- `.qiwo-sync/**`
