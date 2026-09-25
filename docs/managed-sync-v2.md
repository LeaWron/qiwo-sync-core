# Managed sync: v2 standard / v3 compatible

> 历史实验：用户已取消空间迁移方案。当前助手改为直接清理原目录的设备残留，见 [现行方案](device-residual-cleanup.md)。旧迁移任务在实际 helper 中已禁用。


Status: implemented and enabled **only through the updated Linux fcitx5 trial
frontend**. Other platforms retain read-only legacy inventory. An existing account
is never migrated automatically. No real account is changed by building/testing.

## User flow and scope

Restart the whole fcitx5 trial (not just the assistant) after updating. In 齐我助手,
choose **管理同步内容 → 预览迁移 → 核对范围 → 确认执行**. If a managed space already
exists, use the explicit join preview. The panel shows active files, device retirement,
and restorable trash. Every action has its own preview and confirmation. Preserved
previews and uncertain tasks can be reopened and retried with the same operation ID.
A stale state requires a new preview; it is never silently recalculated and executed.

Migration snapshots cloud files in the supported personal layer: custom YAML,
custom_phrase.txt and sync snapshots. It excludes distributed dictionaries, schemas,
internal metadata, user.yaml and installation.yaml. This selector is frozen in
`lifecycle/selector.rs` and shared verbatim with the assistant's older pinned core,
without changing that core's legacy selector. Local-only active files enter on the
next managed sync. Joining initially pulls the managed view, preserving differing
local files in recovery. Unregistered foreign snapshots are quarantined.

Only upgraded/enrolled fcitx5 devices participate. Android, Windows, macOS and IBus
clients continue using the old space; their subsequent changes do not enter v2.
Migration retains the entire old namespace. Trash retains original object references,
so retirement/cleanup **stops participation but does not release cloud storage**.
There is no permanent purge or garbage collector in this release. Old copies and
local recovery/staging files remain recoverable; automatic retention expiry is absent.

## Storage and initialization

Under the configured sync root, `.qiwo-managed-v2/state.json` is the sole authoritative
snapshot. It is disjoint from the v1 manifest and physical `sync/<id>/` paths.
Immutable `objects/<sha256>` payloads are created with If-None-Match: *, fetched back,
and checked against SHA-256 and exact size. Corrupt existing objects cannot be replaced.

A migration preview reads and hashes cloud bytes. Execution re-reads each selected
source and rejects changed content, stages and verifies every object, then exclusively
creates the initial state. This is a snapshot of the previewed content, not a live
bridge: legacy writers can keep changing the old space, but no v1 data is imported
again automatically. A durable initial receipt reconciles an interrupted final response.

Initialization first probes exclusive creation, successful matching-ETag update,
rejection of stale-ETag update, and unchanged contents after conflict. A strong ETag
alone is insufficient. Probe objects are retained for diagnosis; no DELETE is issued.
Servers must maintain these conditional-write guarantees after enrollment.
The compatible transport below uses a separate capability probe and publication path.

## State and commits

All fields use camelCase; standard spaces use protocolVersion 2. Compatible spaces
use protocolVersion 3 plus the required transportProfile. State contains revision, active file
references, exact retiredDevices and deletedPaths, trash, and durable receipts.
Tombstones and receipts never expire automatically. Empty-device retirement is valid.
The `sync/old/` boundary cannot match `sync/older/`.

A lifecycle plan binds actor, operation ID, selection and original references to the
digest of the entire observed state. The assistant persists the exact native request
in `.qiwo-sync/managed-requests/` before remote changes and accepts only its ID at
execution. Native settings/actor are resolved again; state or identity changes abort.

All state writers load a validated snapshot and strong ETag, preserve lifecycle fields,
and PUT one complete state with If-Match. HTTP 412 aborts without overwrite/retry.
A lost reply is an uncertain outcome; retry the original plan. A matching receipt
makes that retry idempotent. Restore refuses an occupied original path and cannot
bypass an independently retired owner. Restoring snapshots does not undo words
already merged into Rime's user database.

Transfer supports full, push, pull and snapshot-only scopes at the core layer. Native
fcitx exposes full sync only. Uploads never implicitly remove remote entries, never
upload foreign-device snapshots, and cannot revive tombstones. Divergent local files
are backed up before accepting cloud content. Downloads are staged and hash-verified;
a changed cloud snapshot aborts before local installation. Baselines advance only
following successful application. Final native import has its own fresh preflight.

## Native coordination and local recovery

The fcitx state machine serializes management, export/import, deploy and ID changes.
A nonblocking shared flock protects the entire native task; its dedicated worker
inherits the same open lock description, so host crashes cannot release the gate
while a worker is still writing. Ordinary native/automatic sync routes through the
managed preflight when enrolled. Network failures stop native import.

Before every Rime sync_user_data (which can both export and import), fetch fresh state
and move blocked or unregistered foreign snapshots out of `sync/`. Copies live under
`.qiwo-sync/managed-recovery/<operation>/<original path>`. A retired local actor is
quarantined and then refused. Recovery and download staging are never synced.

Enrollment is saved atomically before touching snapshots. It binds the normalized
server URL, username, initial migration receipt, current device ID, baseline and
revision. Changed endpoint/account, replaced state, missing state, rollback, malformed
marker, or identity mismatch fails closed. Password rotation is allowed. Direct ID
or server/account changes are unavailable while enrolled. Other Rust/CLI entry points
reject enrolled directories instead of falling back to legacy sync.

Symlinks in managed filesystem paths are rejected. These checks assume a cooperating
same-user process; they are not a security boundary against a malicious process
replacing directories concurrently. No file-management design can retroactively undo
words imported before a remote cleanup; clients apply retirement on their next
successful preflight. Unsupported old binaries must not operate on an enrolled local
profile; this trial does not install or roll back production clients.

## Limits and validation

State and native requests: 4 MiB; object: 64 MiB; migration content: 512 MiB;
10,000 entries per state collection. Exceeding limits fails closed; records are not
trimmed. Remote requests time out and redirects are disabled. Frontend tasks have a
three-minute timeout; uncertain results preserve the request for retry.

- `cargo test --workspace` covers legacy behavior plus lifecycle, CAS, migration,
  server capability failures, corrupt objects, interrupted initialization, two-client
  conflicts, deleted-file resurrection, identity/endpoint protection and symlinks.
- `qiwo-companion/tests/managed-ui.cjs` covers per-operation confirmation, retry and
  stale preview suppression.
- `qiwo-fcitx5/addon/tests/desktop_managed.py` uses a disposable native Rime instance
  and loopback WebDAV for migration → retirement → stale-file reintroduction → sync
  → restoration, plus missing-state and identity guards. Optional
  QIWO_MANAGED_HELPER_HARNESS runs the real assistant command integration too.

Remaining rollout: integrate and validate Android, Windows, macOS and IBus native
export/import and identity paths before allowing them to enroll. Physical garbage
collection and managed identity replacement are separate future work.

## Server compatibility failures

Do not retry capability failures without a server-side compatibility change. The UI
shows the specific backend guidance instead of appending a generic retry instruction.
A failed pre-migration probe may leave dedicated `.qiwo-managed-v2/probes/` files and
empty management directories; no state or content objects are published at this stage.
The original sync files and the local enrollment remain unchanged. Ordinary sync and
read-only inventory remain available for a profile that has not enrolled.

Observed on a configured Jianguoyun endpoint on 2026-09-25: GET returned an unquoted
ETag; an existing diagnostic object accepted PUT with `If-None-Match: *` (201 for
identical bytes, 204 for different bytes). A GET confirmed that different bytes had
overwritten the probe; the diagnostic content was then restored. A deliberately
unmatched `If-Match` returned 412. This endpoint cannot satisfy the
current protocol's exclusive initialization requirement. Adding quotes or relaxing
ETag validation alone is insufficient. This is an observation of this endpoint,
not a claim about every deployment. The strict v2 gate stays in place. The v3 transport experiment below is now disabled for this endpoint after a concurrent CAS counterexample.


## Suspended Jianguoyun transport experiment (2026-09-25)

**Do not migrate or retry managed operations on `dav.jianguoyun.com`.** An isolated
real-server recheck accepted both concurrent PUTs carrying the same old validator
(HTTP 204 / 204). Successful earlier samples do not establish atomicity. The core
now refuses managed writes and transfer before network operations for this host;
the assistant hides migration and saved-task actions. Legacy sync and read-only
inventory remain available. No user data was migrated. The diagnostic fixture was
removed and verified absent. See `validation/jianguoyun-2026-09-25-recheck.json`.
The remaining section describes the suspended implementation, not compatibility
certification. Local atomic-server tests validate only the modeled semantics.

The suspended implementation proposes `jianguoyun-opaque-cas-move-v1` for the canonical
Jianguoyun host. This is only a preview proposal: execution must prove raw-token
CAS, stale rejection, one winner under concurrent CAS, exclusive MOVE publication,
and preservation of both an existing destination and the rejected source. Domain
matching and OPTIONS alone never enable writes. Standard v2 spaces retain their
strict transport; existing spaces are never automatically upgraded.

Compatible states are version 3. Both the state and enrollment bind the profile;
version 3 enrollment adds protocolVersion/transportProfile to the existing
`.qiwo-sync/managed-v2.json` sentinel. The namespace name is retained so old native
frontends still detect enrollment. Older cores reject the new fields or state
version. Native capability is now `qiwo-managed-v3-fcitx5-1`; older assistants
reject it. Old migration plans lack a bound profile and are refused before remote writes.
Restart the trial to see the current suspension message; do not create new Jianguoyun previews.

Opaque validators are a separate type, restricted to 1–256 ASCII alphanumeric,
underscore or hyphen bytes. Empty, weak, wildcard, quoted, whitespace and multi-value
tokens are refused; tokens are never quoted, stripped or synthesized. GET obtains
state and its validator together; every update uses the exact If-Match and retains
all lifecycle records. A 412 aborts; an uncertain result retains the original task.

Final objects and the initial state are never PUT unconditionally in this profile.
Upload to `staging/<CSPRNG id>`, GET and hash-check, then MOVE with explicit
`Overwrite: F`. GET the destination to reconcile 201, 409/412 or a lost MOVE reply.
Only exact object bytes or the same migration receipt prove success. Never switch
to overwrite mode. Endpoint/target/content-bound upload journals are private local
files in `.qiwo-sync/managed-uploads/`; retry reuses the durable staging ID. Corrupt
existing objects and staging files are not replaced. Probe files and uncertain
staging/journals remain for diagnosis; physical garbage collection is still absent.

Before activation, successful probe evidence (endpoint/account fingerprint, profile,
probe version and timestamp) is recorded locally. Migration and joining run fresh
probes; ordinary managed sync uses the bound profile without creating new probes.
Network/identity/state failures continue to stop native import and legacy fallback.

A directory response with 750 or more response records is conservatively incomplete,
covering the documented Jianguoyun page boundary (including a collection's own
record). Migration is blocked in that case. Pagination is not implemented; small
listings were exercised against the real endpoint, and the cutoff has a regression.

Validation: root workspace regressions and strict clippy; both linked cores' managed
transport/lifecycle/inventory tests; assistant tests, UI suites and strict clippy;
real fcitx/Rime with standard and compatible local DAV semantics; previous helper
binary and previous assistant command harness reject the upgrade. The opt-in
`managed_live` test exercised the actual Rust engine against a disposable real
Jianguoyun fixture: migrate, retry, upload, second-device join, retirement, offline
resurrection refusal and restore. The wrapper verified removal of that entire
fixture. No production sync data was migrated or cleaned.

Run the opt-in fixture only via `tools/test_dav_compatible_live.py` in the main core
checkout; it requires an existing probes parent, restricts the remote root to a
fresh owned UUID directory, uses synthetic bytes, and cleans only that directory.
