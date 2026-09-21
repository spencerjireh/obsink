// IndexedDB is the browser's `~/.obsink`: vault entries and the bearer (as
// app.json + the keychain do on desktop), the picked directory handles, and
// per-vault sync bookkeeping (the base manifest, the remote-manifest cache
// and the hash cache, which desktop keeps under `.obsink/` in the folder)
// plus the activity log. Nothing here holds a passphrase or a derived key.

export const DB_NAME = 'obsink'
const DB_VERSION = 1

export type Store = 'kv' | 'vaults' | 'handles' | 'state' | 'activity'
const STORES: Store[] = ['kv', 'vaults', 'handles', 'state', 'activity']

// A configured vault (desktop `StoredVault`); `folder_name` is what the UI
// prints where desktop prints the path.
export type StoredVault = {
  id: string
  name: string
  server_url: string
  handle_id: string
  folder_name: string
  ignore: string[]
  created: number
}

export type FileEntry = {
  hash: string
  modified: number
  size: number
  deleted: boolean
  encPath: string
}
export type Manifest = Record<string, FileEntry>

export type Stat = { mtime_secs: number; mtime_nanos: number; size: number }
export type HashCache = { key_id: string; entries: Record<string, { stat: Stat; hash: string }> }

// Per-vault sync bookkeeping.
export type VaultSyncState = {
  base: Manifest
  remote_cache: { etag: string | null; manifest: Manifest } | null
  hash_cache: HashCache | null
  last_synced: number | null
}

let opening: Promise<IDBDatabase> | null = null

function request<T>(req: IDBRequest<T>): Promise<T> {
  return new Promise((resolve, reject) => {
    req.onsuccess = () => resolve(req.result)
    req.onerror = () => reject(req.error ?? new Error('IndexedDB request failed'))
  })
}

function openAt(version: number | undefined): Promise<IDBDatabase> {
  return new Promise((resolve, reject) => {
    const req = version === undefined ? indexedDB.open(DB_NAME) : indexedDB.open(DB_NAME, version)
    req.onupgradeneeded = () => {
      const db = req.result
      for (const store of STORES) {
        if (!db.objectStoreNames.contains(store)) db.createObjectStore(store)
      }
    }
    req.onsuccess = () => resolve(req.result)
    req.onerror = () => reject(req.error ?? new Error('could not open IndexedDB'))
  })
}

export function openDb(): Promise<IDBDatabase> {
  if (opening) return opening
  opening = (async () => {
    let db = await openAt(DB_VERSION)
    // A database someone else created without our stores (a harness, an
    // older page) is upgraded in place rather than left unusable.
    if (!STORES.every((store) => db.objectStoreNames.contains(store))) {
      const next = db.version + 1
      db.close()
      db = await openAt(next)
    }
    // A version change elsewhere (another tab upgrading) closes this one.
    db.onversionchange = () => {
      db.close()
      opening = null
    }
    return db
  })().catch((error) => {
    opening = null
    throw error
  })
  return opening
}

async function tx(store: Store, mode: IDBTransactionMode): Promise<IDBObjectStore> {
  const db = await openDb()
  return db.transaction(store, mode).objectStore(store)
}

export async function get<T>(store: Store, key: string): Promise<T | undefined> {
  return request<T | undefined>((await tx(store, 'readonly')).get(key))
}

export async function put<T>(store: Store, key: string, value: T): Promise<void> {
  await request((await tx(store, 'readwrite')).put(value, key))
}

export async function del(store: Store, key: string): Promise<void> {
  await request((await tx(store, 'readwrite')).delete(key))
}

export async function all<T>(store: Store): Promise<T[]> {
  return request<T[]>((await tx(store, 'readonly')).getAll())
}

export async function keys(store: Store): Promise<string[]> {
  const found = await request((await tx(store, 'readonly')).getAllKeys())
  return found.map(String)
}

// Vault entries in the order they were added (desktop keeps config order).
export async function listVaults(): Promise<StoredVault[]> {
  const vaults = await all<StoredVault>('vaults')
  return vaults.sort((a, b) => a.created - b.created)
}

export const KV_ACTIVE_VAULT = 'active_vault'

export function bearerKey(serverUrl: string): string {
  return `bearer:${serverUrl}`
}

export function emptySyncState(): VaultSyncState {
  return { base: {}, remote_cache: null, hash_cache: null, last_synced: null }
}
