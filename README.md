# qiwo-sync-core

`qiwo-sync-core` is the shared sync component for Qiwo Rime frontends.

The first version is intentionally small:

- sync selected Rime config files through WebDAV
- support `sync`, `push`, `pull`, and `init-frost`
- keep a local manifest under `.qiwo-sync/manifest.json`
- create local conflict backups before overwriting files
- avoid generated Rime build artifacts and binary user databases

## CLI

```powershell
dotnet run --project qiwo-sync-core/src/qiwo-rime-sync -- sync `
  --frontend weasel `
  --rime-user-dir "$env:APPDATA\Rime" `
  --remote-url "https://dav.example.com/qiwo-rime-sync" `
  --username "name" `
  --password-env QIWO_WEBDAV_PASSWORD `
  --device-id "windows-main"
```

Initialize `rime-frost` into a Rime user directory:

```powershell
dotnet run --project qiwo-sync-core/src/qiwo-rime-sync -- init-frost `
  --rime-user-dir "$env:APPDATA\Rime" `
  --frost-dir ".\rime-frost"
```

## Synced files

Included:

- `*.custom.yaml`
- `*.schema.yaml`
- `*.dict.yaml`
- `custom_phrase.txt`
- `symbols.yaml`
- `opencc/**`
- `lua/**`

Excluded:

- `build/**`
- `*.bin`
- `*.table.bin`
- `*.reverse.bin`
- `*.userdb/**`
- `.git/**`
- `.qiwo-sync/**`
