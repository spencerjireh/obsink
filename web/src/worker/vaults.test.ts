import 'fake-indexeddb/auto'
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest'

// The worker runs in a browser; these tests stand in the globals it reads at
// import time and stub `fetch`, then exercise the device id and the merged
// vault list (spec §15.1) without the wasm engine: a stored vault without a
// key in memory reads `locked`, so no folder or manifest is touched.

vi.stubGlobal('self', {
  location: { origin: 'http://localhost' },
  navigator: { platform: 'MacIntel' },
})

const { del, put } = await import('../shared/db')
const session = await import('./session')
const vaults = await import('./vaults')

let fetchMock: ReturnType<typeof vi.fn>

function json(status: number, body: unknown): Response {
  return new Response(JSON.stringify(body), {
    status,
    headers: { 'Content-Type': 'application/json' },
  })
}

beforeEach(() => {
  fetchMock = vi.fn()
  vi.stubGlobal('fetch', fetchMock)
})

afterEach(() => {
  vi.unstubAllGlobals()
  vi.stubGlobal('self', {
    location: { origin: 'http://localhost' },
    navigator: { platform: 'MacIntel' },
  })
})

describe('the device id', () => {
  it('is minted once and kept in the kv store', async () => {
    const first = await session.loadOrCreateDeviceId()
    expect(first).toMatch(/^dev_[0-9a-f]{32}$/)
    expect(await session.loadOrCreateDeviceId()).toBe(first)
    const device = await session.thisDevice()
    expect(device).toEqual({ id: first, name: 'MacIntel (ObSink Web)', platform: 'browser' })
  })
})

describe('the merged vault list', () => {
  it('joins the server list with the stored vaults', async () => {
    await put('kv', 'bearer:http://localhost', 'os_token')
    await put('vaults', 'vault_here', {
      id: 'vault_here',
      name: 'Here (stale name)',
      handle_id: 'h-1',
      folder_name: 'Here',
      ignore: [],
      created: 1,
    })
    await put('vaults', 'vault_gone', {
      id: 'vault_gone',
      name: 'Gone',
      handle_id: 'h-2',
      folder_name: 'Gone',
      ignore: [],
      created: 2,
    })
    fetchMock.mockResolvedValue(
      json(200, {
        vaults: [
          {
            id: 'vault_here',
            name: 'Here',
            created: 1,
            revision: 4,
            bytes: 100,
            wrapped_key: 'AAAA',
            devices: [{ id: 'dev-1', name: 'Mac', platform: 'macos', last_revision: 4 }],
          },
          { id: 'vault_elsewhere', name: 'Elsewhere', created: 1, revision: 2, bytes: 7 },
        ],
      }),
    )
    const listed = await vaults.listVaults()
    expect(listed.map((entry) => [entry.id, entry.state.kind, entry.local_path])).toEqual([
      ['vault_here', 'locked', 'Here'],
      ['vault_elsewhere', 'not_on_device', null],
      ['vault_gone', 'deleted_on_server', 'Gone'],
    ])
    const here = listed[0]
    expect(here.name).toBe('Here')
    expect(here.revision).toBe(4)
    expect(here.devices).toEqual([
      { id: 'dev-1', name: 'Mac', platform: 'macos', last_synced: null, last_revision: 4 },
    ])
  })

  it('lists the stored vaults as locked when signed out', async () => {
    await del('kv', 'bearer:http://localhost')
    const listed = await vaults.listVaults()
    expect(listed.length).toBeGreaterThan(0)
    expect(listed.every((entry) => entry.state.kind === 'locked')).toBe(true)
    expect(fetchMock).not.toHaveBeenCalled()
  })
})
