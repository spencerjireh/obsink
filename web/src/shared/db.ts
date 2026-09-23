// IndexedDB is the browser's `~/.obsink`: vault entries, the bearer and the
// device id (as app.json + the keychain do on desktop), the picked directory
// handles, and per-vault sync bookkeeping (the base manifest, the
// remote-manifest cache and the hash cache, which desktop keeps under
// `.obsink/` in the folder) plus the activity log. Nothing here holds a
// passphrase or a key: the account key and the vault keys live in the
// worker's memory for the tab's lifetime (spec §6.3).

export const DB_NAME = 'obsink'
// v2 (wire format v3): vault entries lose `server_url` and the `active_vault`
// key goes (every page talks to its own origin; the list has no active vault).
export const DB_VERSION = 2

export type Store = 'kv' | 'vaults' | 'handles' | 'state' | 'activity'
const STORES: Store[] = ['kv', 'vaults', 'handles', 'state', 'activity']

// A configured vault (desktop `StoredVault`); `folder_name` is what the UI
// prints where desktop prints the path.
export type StoredVault = {
  id: string
  name: string
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
    req.onupgradeneeded = (event) => {
      const db = req.result
      for (const store of STORES) {
        if (!db.objectStoreNames.contains(store)) db.createObjectStore(store)
      }
      if (event.oldVersion > 0 && event.oldVersion < 2) migrateToV2(req.transaction)
    }
    req.onsuccess = () => resolve(req.result)
    req.onerror = () => reject(req.error ?? new Error('could not open IndexedDB'))
  })
}

// The v1 -> v2 rewrite, inside the upgrade transaction: every vault entry
// without its `server_url`, and no `active_vault` key.
function migrateToV2(transaction: IDBTransaction | null): void {
  if (!transaction) return
  const vaults = transaction.objectStore('vaults')
  vaults.openCursor().onsuccess = (event) => {
    const cursor = (event.target as IDBRequest<IDBCursorWithValue | null>).result
    if (!cursor) return
    const rest = { ...(cursor.value as StoredVault & { server_url?: string }) }
    delete rest.server_url
    cursor.update(rest)
    cursor.continue()
  }
  transaction.objectStore('kv').delete('active_vault')
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
  try {
    await request((await tx(store, 'readwrite')).put(value, key))
  } catch (error) {
    // The one storage failure a user can act on gets a sentence, not a DOMException name.
    if ((error as DOMException)?.name === 'QuotaExceededError') {
      throw {
        kind: 'other',
        message: 'The browser is out of storage for ObSink. Free some space and try again.',
      }
    }
    throw error
  }
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

// This browser profile's device id (spec §4.1), generated once. A cleared
// profile (site data removed, a new browser) is a new device: the old one
// stays on the Devices tab until it is signed out there.
export const KV_DEVICE_ID = 'device_id'

export function bearerKey(serverUrl: string): string {
  return `bearer:${serverUrl}`
}

export function emptySyncState(): VaultSyncState {
  return { base: {}, remote_cache: null, hash_cache: null, last_synced: null }
}
