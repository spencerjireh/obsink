import 'fake-indexeddb/auto'
import { describe, expect, it } from 'vitest'
import { del, get, listVaults, put, type StoredVault } from './db'

function vault(id: string, created: number): StoredVault {
  return {
    id,
    name: id,
    handle_id: `h-${id}`,
    folder_name: id,
    ignore: [],
    created,
  }
}

describe('db', () => {
  it('round-trips values by key', async () => {
    await put('kv', 'bearer:http://localhost', 'os_token')
    expect(await get<string>('kv', 'bearer:http://localhost')).toBe('os_token')
    await del('kv', 'bearer:http://localhost')
    expect(await get('kv', 'bearer:http://localhost')).toBeUndefined()
  })

  it('lists vaults in the order they were added', async () => {
    await put('vaults', 'vault_b', vault('vault_b', 2))
    await put('vaults', 'vault_a', vault('vault_a', 1))
    expect((await listVaults()).map((entry) => entry.id)).toEqual(['vault_a', 'vault_b'])
  })
})
