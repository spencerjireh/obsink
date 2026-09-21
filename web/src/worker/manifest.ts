import {
  emptySyncState,
  get,
  put,
  type FileEntry,
  type HashCache,
  type Manifest,
  type StoredVault,
  type VaultSyncState,
} from '../shared/db'
import type { WireFileEntry } from './api'
import { readFile, scan, statOf, type ScannedFile } from './fs'
import { api, bearerCall } from './session'
import { wasm, type Core, type VaultKeys } from './wasm'

// The read side of a sync cycle (core `load_local_state`,
// `fetch_remote_manifest`, `diff_local_and_remote`): the working manifest
// of the folder, the server manifest through its ETag cache, and the
// three-way diff against the base of the last completed sync.

export type ManifestDiff = {
  upload: SyncAction[]
  download: SyncAction[]
  conflicts: Conflict[]
}
export type SyncActionKind = 'Upload' | 'Download' | 'DeleteLocal' | 'DeleteRemote'
export type SyncAction = {
  path: string
  kind: SyncActionKind
  local: FileEntry | null
  remote: FileEntry | null
}
export type Conflict = { path: string; local: FileEntry; remote: FileEntry }

export function loadSyncState(vaultId: string): Promise<VaultSyncState> {
  return get<VaultSyncState>('state', vaultId).then((state) => state ?? emptySyncState())
}

export function saveSyncState(vaultId: string, state: VaultSyncState): Promise<void> {
  return put('state', vaultId, state)
}

export async function ignoreFor(vault: StoredVault): Promise<InstanceType<Core['Ignore']>> {
  const core = await wasm()
  return new core.Ignore(JSON.stringify(vault.ignore))
}

function nowSeconds(): number {
  return Math.floor(Date.now() / 1000)
}

// The folder as a manifest: cached hashes for unchanged files, an HMAC for
// the rest, and a tombstone for every base entry no longer on disk (it
// keeps the base hash, the parent hash the server checks on delete).
export async function buildWorkingManifest(
  root: FileSystemDirectoryHandle,
  files: ScannedFile[],
  keys: VaultKeys,
  cache: HashCache | null,
  base: Manifest,
): Promise<{ working: Manifest; cache: HashCache }> {
  const keyId = keys.hashCacheKeyId()
  const previous = cache && cache.key_id === keyId ? cache.entries : {}
  const next: HashCache = { key_id: keyId, entries: {} }
  const working: Manifest = {}
  for (const file of files) {
    const stat = statOf(file)
    const memo = previous[file.path]
    let hash: string
    if (
      memo &&
      memo.stat.mtime_secs === stat.mtime_secs &&
      memo.stat.mtime_nanos === stat.mtime_nanos &&
      memo.stat.size === stat.size
    ) {
      hash = memo.hash
    } else {
      hash = keys.contentHmac(await readFile(root, file.path))
    }
    next.entries[file.path] = { stat, hash }
    working[file.path] = {
      hash,
      modified: stat.mtime_secs,
      size: stat.size,
      deleted: false,
      encPath: '',
    }
  }
  for (const [path, entry] of Object.entries(base)) {
    if (!(path in working) && !entry.deleted) {
      working[path] = { ...entry, deleted: true, modified: nowSeconds() }
    }
  }
  return { working, cache: next }
}

// Re-key the wire manifest (path tokens) by real path; entries without an
// `encPath` are unreadable and skipped, a failed decrypt is a wrong key.
export function decodeManifest(wire: Record<string, WireFileEntry>, keys: VaultKeys): Manifest {
  const manifest: Manifest = {}
  for (const entry of Object.values(wire)) {
    if (!entry.encPath) continue
    const path = keys.decryptPath(entry.encPath)
    manifest[path] = {
      hash: entry.hash,
      modified: entry.modified,
      size: entry.size,
      deleted: entry.deleted ?? false,
      encPath: entry.encPath,
    }
  }
  return manifest
}

// The server manifest, through the ETag cache: an unchanged manifest costs
// a 304. Updates `state.remote_cache` in place (the caller persists).
export async function fetchRemoteManifest(
  vault: StoredVault,
  keys: VaultKeys,
  state: VaultSyncState,
): Promise<Manifest> {
  const cached = state.remote_cache
  const fetched = await bearerCall((bearer) =>
    api.getManifest(bearer, vault.id, cached?.etag ?? null),
  )
  if (fetched.status === 'not_modified' && cached) return cached.manifest
  if (fetched.status === 'not_modified') {
    // A 304 without a cache cannot happen (no If-None-Match was sent).
    throw new Error('server answered 304 without a cached manifest')
  }
  const manifest = decodeManifest(fetched.manifest, keys)
  state.remote_cache = fetched.etag ? { etag: fetched.etag, manifest } : null
  return manifest
}

// `true` when the server manifest moved since the cache was written.
export async function remoteChanged(
  vault: StoredVault,
  keys: VaultKeys,
  state: VaultSyncState,
): Promise<boolean> {
  const cached = state.remote_cache
  if (!cached) return true
  const fetched = await bearerCall((bearer) => api.getManifest(bearer, vault.id, cached.etag))
  if (fetched.status === 'not_modified') return false
  const manifest = decodeManifest(fetched.manifest, keys)
  state.remote_cache = fetched.etag ? { etag: fetched.etag, manifest } : null
  return true
}

function withoutIgnored(manifest: Manifest, ignore: InstanceType<Core['Ignore']>): Manifest {
  const kept: Manifest = {}
  for (const [path, entry] of Object.entries(manifest)) {
    if (!ignore.isIgnored(path)) kept[path] = entry
  }
  return kept
}

export async function diffManifests(
  base: Manifest,
  working: Manifest,
  remote: Manifest,
  ignore: InstanceType<Core['Ignore']>,
): Promise<ManifestDiff> {
  const core = await wasm()
  return JSON.parse(
    core.diffManifests(
      JSON.stringify(withoutIgnored(base, ignore)),
      JSON.stringify(withoutIgnored(working, ignore)),
      JSON.stringify(withoutIgnored(remote, ignore)),
    ),
  ) as ManifestDiff
}

export type LocalState = {
  state: VaultSyncState
  files: ScannedFile[]
  working: Manifest
  ignore: InstanceType<Core['Ignore']>
}

// Base + hash cache from IndexedDB, the folder walked and hashed.
export async function loadLocalState(
  vault: StoredVault,
  root: FileSystemDirectoryHandle,
  keys: VaultKeys,
): Promise<LocalState> {
  const state = await loadSyncState(vault.id)
  const ignore = await ignoreFor(vault)
  const files = await scan(root, ignore)
  const { working, cache } = await buildWorkingManifest(
    root,
    files,
    keys,
    state.hash_cache,
    state.base,
  )
  state.hash_cache = cache
  return { state, files, working, ignore }
}
