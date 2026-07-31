# P4 — iOS + File Provider: execution plan

> Status source of truth is the Plane project **OBS** (module P4). This doc is the
> engineering breakdown; `spec.md` §11 is the File Provider design spec.

P4 delivers real cross-device Obsidian sync on iOS. It's broken into **six
dependency-ordered slices** (one session-ish each, with a verification gate before
the next). The File Provider extension **never touches the network** (spec §11.4) —
it's a passive view over a shared SQLite DB + on-disk cache in the App Group; the
app is the sole sync driver.

## Cross-cutting decisions

- **SQLite wrapper:** [GRDB.swift](https://github.com/groue/GRDB.swift) **v6.29**
  via Swift Package Manager (`from: "6.29.0"`). `SWIFT_VERSION` bumped to `5.9`.
- **Shared code:** the DB layer lives in `ios/DB/`, compiled into **both** the
  `ObSink` app and the `FileProviderExt` extension via `project.yml` sources.
- **DB location:** `group.com.obsink.shared/obsink.sqlite`, opened with WAL.
- **Identifiers:** stable UUIDs (never paths); UUID↔path map in the DB (spec §11.6).
- **No Rust changes until Slice E.** Slices A–D are pure Swift/XcodeGen.

## Slice A — Item DB foundation (OBS-7, 8, 9, 10, 18)

GRDB + shared `ItemStore` + the `items` schema (spec §11.5) + UUID-keyed CRUD +
`reconcile(vaultRoot:)` scan. Unblocks B–D; no `mobile/` change.

Schema (`v1_items`):
```sql
CREATE TABLE items (
  identifier       TEXT PRIMARY KEY,          -- stable UUID
  parentIdentifier TEXT NOT NULL,             -- parent UUID (root => "")
  filename         TEXT NOT NULL,
  contentHash      TEXT,
  localPath        TEXT NOT NULL,             -- vault-relative, e.g. "notes/a.md"
  isDirectory      INTEGER NOT NULL DEFAULT 0,
  size             INTEGER,
  modified         INTEGER NOT NULL DEFAULT 0,
  pendingUpload    INTEGER NOT NULL DEFAULT 0,
  pendingDeletion  INTEGER NOT NULL DEFAULT 0,
  rowVersion       INTEGER NOT NULL DEFAULT 0 -- monotonic; drives enumerateChanges
);
CREATE INDEX items_parent ON items(parentIdentifier);
CREATE INDEX items_rowVersion ON items(rowVersion);
```
Plus a `meta(key, value)` KV holding `syncAnchor` = max(rowVersion) after reconcile.

**Gate:** `xcodebuild test -scheme ObSink -sdk iphonesimulator` green (new
`ItemStoreTests`).

## Slice B — File Provider goes DB-backed + real deltas (OBS-11, 12, 13, 14, 15, 16, 17)

Migrates the FP fully to UUID identifiers backed by `ItemStore` (read **and** write
paths — you can't half-migrate): `item(for:)`/`fetchContents` serve from DB + cache
(OBS-13); `createItem`/`modifyItem`/`deleteItem` resolve UUID↔path via the DB and set
`pendingUpload`/`pendingDeletion` (OBS-14/15/16); a monotonic `rowVersion` anchor
(OBS-17) plus an `isDeleted` tombstone let `enumerateChanges` report real
insert/modify/delete deltas (OBS-12). App-side draining of the pending flags stays
in Slice D (OBS-22/23).

**Gate:** seeded items appear via `enumerateChanges`; mutation yields exactly that delta.

## Slice C — App→DB→FP bridge + static validation (OBS-19, 20, 21)

After sync, the app calls `ItemStore.reconcile(vaultRoot:)` to upsert downloaded/
changed files (OBS-20), then `NSFileProviderManager.signalEnumerator(for: .workingSet)`
(OBS-21). Static validation checkpoint: seeded files appear in iOS Files + Obsidian
(sim) opens the folder as a vault (OBS-19).

## Slice D — FP write-back draining (OBS-22, 23)

The app's next sync drains the `pendingUpload`/`pendingDeletion` flags the FP set in
Slice B: uploads/deletes flow through the core (which already scans the vault dir),
then the app clears the flags. Pending count drives a "Local changes — Sync" UI affordance.

## Slice E — Keychain, multi-vault, conflict-UI polish (OBS-24, 25, 26, 27, 28)

- **Keychain (OBS-27):** `KeychainStore` for the derived key; add
  `keychain-access-groups` to the app entitlement.
- **Multi-vault (OBS-28):** facade gains `list_vaults`/`create_vault`; app gains a
  vault picker + multi-vault store.
- **Conflict UI (OBS-24/25/26):** facade gains `conflict_preview(path)`; detail view
  with segmented This/Other device, read-only previews, modified timestamps
  (`MobileConflict` already carries `*_modified`).

**This is the first slice that touches `mobile/src/lib.rs`.**

## Slice F — Cross-device E2E + TestFlight (OBS-29, 30, 31, 32, 33, 34)

Mac↔iOS scenarios: create / edit / conflict / delete / stale-vault banner against a
shared vault; real Obsidian vault (plugins, images, `.obsidian/`).

> **Partly manual:** TestFlight needs `DEVELOPMENT_TEAM` + a signing cert +
> upload via Xcode — an out-of-band Apple-side step, not fully automatable.

## Dependency graph

```
A (DB) ─▶ B (FP DB+delta) ─▶ C (bridge) ─▶ D (write-back)
                                ─▶ E (keychain/vault/conflict UI) ─▶ F (E2E/TestFlight)
```
E depends on A only, so it can overlap B–D if desired. Recommended order: **A→B→C→D→E→F**.
