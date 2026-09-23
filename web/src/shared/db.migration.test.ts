import 'fake-indexeddb/auto'
import { describe, expect, it } from 'vitest'

// The v1 -> v2 upgrade (wire format v3): a profile from an older build keeps
// its vault entries without `server_url`, and the `active_vault` key goes.

function openV1(): Promise<IDBDatabase> {
  return new Promise((resolve, reject) => {
    const req = indexedDB.open('obsink', 1)
    req.onupgradeneeded = () => {
      for (const store of ['kv', 'vaults', 'handles', 'state', 'activity']) {
        req.result.createObjectStore(store)
      }
    }
    req.onsuccess = () => resolve(req.result)
    req.onerror = () => reject(req.error)
  })
}

function write(db: IDBDatabase, store: string, key: string, value: unknown): Promise<void> {
  return new Promise((resolve, reject) => {
    const tx = db.transaction(store, 'readwrite')
    tx.objectStore(store).put(value, key)
    tx.oncomplete = () => resolve()
    tx.onerror = () => reject(tx.error)
  })
}

describe('db migration', () => {
  it('drops server_url and active_vault from a v1 database', async () => {
    const v1 = await openV1()
    await write(v1, 'vaults', 'vault_a', {
      id: 'vault_a',
      name: 'A',
      server_url: 'http://old.example',
      handle_id: 'h-a',
      folder_name: 'A',
      ignore: ['drafts/'],
      created: 1,
    })
    await write(v1, 'kv', 'active_vault', 'vault_a')
    await write(v1, 'kv', 'bearer:http://localhost', 'os_token')
    v1.close()

    const db = await import('./db')
    const vaults = await db.listVaults()
    expect(vaults).toEqual([
      {
        id: 'vault_a',
        name: 'A',
        handle_id: 'h-a',
        folder_name: 'A',
        ignore: ['drafts/'],
        created: 1,
      },
    ])
    expect(await db.get('kv', 'active_vault')).toBeUndefined()
    expect(await db.get('kv', 'bearer:http://localhost')).toBe('os_token')
    expect((await db.openDb()).version).toBe(db.DB_VERSION)
  })
})
