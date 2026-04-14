import { describe, expect, it } from 'vitest'

import worker, { internal, type Env } from '../src/index'

class FakeKVNamespace {
  private readonly store = new Map<string, string>()

  async get(key: string, type?: 'json'): Promise<any> {
    const value = this.store.get(key)
    if (value == null) {
      return null
    }
    return type === 'json' ? JSON.parse(value) : value
  }

  async put(key: string, value: string): Promise<void> {
    this.store.set(key, value)
  }
}

type StoredObject = { key: string; body: Uint8Array }

class FakeR2Bucket {
  private readonly store = new Map<string, Uint8Array>()

  has(key: string): boolean {
    return this.store.has(key)
  }

  keys(): string[] {
    return Array.from(this.store.keys()).sort((a, b) => a.localeCompare(b))
  }

  async get(key: string): Promise<any> {
    const body = this.store.get(key)
    if (!body) {
      return null
    }

    return {
      key,
      body,
      async arrayBuffer() {
        return body.buffer.slice(body.byteOffset, body.byteOffset + body.byteLength)
      },
    }
  }

  async put(key: string, value: ArrayBuffer | ArrayBufferView | ReadableStream | string | null): Promise<void> {
    if (value == null) {
      this.store.set(key, new Uint8Array())
      return
    }

    if (typeof value === 'string') {
      this.store.set(key, new TextEncoder().encode(value))
      return
    }

    if (value instanceof ReadableStream) {
      throw new Error('ReadableStream not supported in fake bucket')
    }

    const bytes = value instanceof ArrayBuffer
      ? new Uint8Array(value)
      : new Uint8Array(value.buffer.slice(value.byteOffset, value.byteOffset + value.byteLength))
    this.store.set(key, bytes)
  }

  async delete(key: string): Promise<void> {
    this.store.delete(key)
  }

  async list(options?: { prefix?: string; cursor?: string }): Promise<any> {
    const prefix = options?.prefix ?? ''
    const objects: StoredObject[] = Array.from(this.store.entries())
      .filter(([key]) => key.startsWith(prefix))
      .map(([key, body]) => ({ key, body }))
      .sort((a, b) => a.key.localeCompare(b.key))

    return {
      objects,
      truncated: false,
      cursor: undefined,
      delimitedPrefixes: [],
    }
  }
}

function createEnv(): Env {
  return {
    API_KEY: 'secret',
    META: new FakeKVNamespace() as unknown as KVNamespace,
    FILES: new FakeR2Bucket() as unknown as R2Bucket,
    MAX_BATCH_INLINE_BYTES: String(50 * 1024 * 1024),
  }
}

function createRequest(input: string, init?: RequestInit): Request<unknown, IncomingRequestCfProperties<unknown>> {
  return new Request(input, init) as Request<unknown, IncomingRequestCfProperties<unknown>>
}

function createContext(): ExecutionContext {
  return {
    waitUntil() {},
    passThroughOnException() {},
    props: {},
  }
}

describe('worker', () => {
  it('rejects unauthorized requests', async () => {
    const response = await worker.fetch(createRequest('https://example.com/vaults'), createEnv(), createContext())

    expect(response.status).toBe(401)
  })

  it('creates vaults and lists them', async () => {
    const env = createEnv()
    const ctx = createContext()

    const createResponse = await worker.fetch(
      createRequest('https://example.com/vaults', {
        method: 'POST',
        headers: {
          Authorization: 'Bearer secret',
          'Content-Type': 'application/json',
        },
        body: JSON.stringify({ name: 'main' }),
      }),
      env,
      ctx,
    )

    expect(createResponse.status).toBe(201)

    const listResponse = await worker.fetch(
      createRequest('https://example.com/vaults', {
        headers: { Authorization: 'Bearer secret' },
      }),
      env,
      ctx,
    )

    const vaults = await listResponse.json<any[]>()
    expect(vaults).toHaveLength(1)
    expect(vaults[0].name).toBe('main')
  })

  it('returns manifests and file blobs for an existing vault', async () => {
    const env = createEnv()
    const ctx = createContext()

    const createResponse = await worker.fetch(
      createRequest('https://example.com/vaults', {
        method: 'POST',
        headers: {
          Authorization: 'Bearer secret',
          'Content-Type': 'application/json',
        },
        body: JSON.stringify({ name: 'main' }),
      }),
      env,
      ctx,
    )

    const { vault } = await createResponse.json<{ vault: { id: string } }>()
    await internal.putFile(
      createRequest('https://example.com', {
        method: 'PUT',
        headers: { 'X-Content-Hash': 'hash-1' },
        body: new TextEncoder().encode('hello worker'),
      }),
      env,
      vault.id,
      'notes/today.md',
    )

    const manifestResponse = await worker.fetch(
      createRequest(`https://example.com/vaults/${vault.id}/manifest`, {
        headers: { Authorization: 'Bearer secret' },
      }),
      env,
      ctx,
    )
    expect(manifestResponse.status).toBe(200)
    const manifest = await manifestResponse.json<Record<string, { hash: string }>>()
    expect(manifest['notes/today.md'].hash).toBe('hash-1')

    const fileResponse = await worker.fetch(
      createRequest(`https://example.com/vaults/${vault.id}/files/${encodeURIComponent('notes/today.md')}`, {
        headers: { Authorization: 'Bearer secret' },
      }),
      env,
      ctx,
    )
    expect(fileResponse.status).toBe(200)
    expect(await fileResponse.text()).toBe('hello worker')
  })

  it('returns conflict on stale parent hash', async () => {
    const env = createEnv()
    const vault = await internal.createVault(
      createRequest('https://example.com/vaults', {
        method: 'POST',
        headers: { 'Content-Type': 'application/json' },
        body: JSON.stringify({ name: 'main' }),
      }),
      env,
    )

    const first = await internal.putFile(
      createRequest('https://example.com', {
        method: 'PUT',
        headers: { 'X-Content-Hash': 'hash-1' },
        body: new TextEncoder().encode('first'),
      }),
      env,
      vault.vault.id,
      'note.md',
    )
    expect(first.status).toBe(200)

    const second = await internal.putFile(
      createRequest('https://example.com', {
        method: 'PUT',
        headers: {
          'X-Parent-Hash': 'stale',
          'X-Content-Hash': 'hash-2',
        },
        body: new TextEncoder().encode('second'),
      }),
      env,
      vault.vault.id,
      'note.md',
    )

    expect(second.status).toBe(409)
    const conflict = await second.json<any>()
    expect(conflict.current.hash).toBe('hash-1')
  })

  it('soft deletes files into trash and marks manifest entries as deleted', async () => {
    const env = createEnv()
    const vault = await internal.createVault(
      createRequest('https://example.com/vaults', {
        method: 'POST',
        headers: { 'Content-Type': 'application/json' },
        body: JSON.stringify({ name: 'main' }),
      }),
      env,
    )
    const bucket = env.FILES as unknown as FakeR2Bucket

    await internal.putFile(
      createRequest('https://example.com', {
        method: 'PUT',
        headers: { 'X-Content-Hash': 'hash-1' },
        body: new TextEncoder().encode('first'),
      }),
      env,
      vault.vault.id,
      'note.md',
    )

    const response = await internal.deleteFile(
      createRequest('https://example.com', {
        method: 'DELETE',
        headers: { 'X-Parent-Hash': 'hash-1' },
      }),
      env,
      vault.vault.id,
      'note.md',
    )

    expect(response.status).toBe(200)
    expect(bucket.has(`${vault.vault.id}/note.md`)).toBe(false)
    expect(bucket.keys().some((key) => key.startsWith(`_trash/${vault.vault.id}/note.md/`))).toBe(true)

    const manifest = await internal.getManifest(env, vault.vault.id)
    expect(manifest['note.md']).toMatchObject({
      hash: 'hash-1',
      deleted: true,
      size: 5,
    })
  })

  it('rejects uploads larger than the configured max file size', async () => {
    const env = createEnv()
    const ctx = createContext()
    const createResponse = await worker.fetch(
      createRequest('https://example.com/vaults', {
        method: 'POST',
        headers: {
          Authorization: 'Bearer secret',
          'Content-Type': 'application/json',
        },
        body: JSON.stringify({ name: 'tiny', max_file_size: 4 }),
      }),
      env,
      ctx,
    )
    const { vault } = await createResponse.json<{ vault: { id: string } }>()

    const response = await worker.fetch(
      createRequest(`https://example.com/vaults/${vault.id}/files/large.bin`, {
        method: 'PUT',
        headers: {
          Authorization: 'Bearer secret',
          'X-Content-Hash': 'hash-1',
        },
        body: new TextEncoder().encode('12345'),
      }),
      env,
      ctx,
    )

    expect(response.status).toBe(413)
    expect(await response.json()).toEqual({ error: 'file too large' })
  })

  it('batch handles mixed results', async () => {
    const env = createEnv()
    const vault = await internal.createVault(
      createRequest('https://example.com/vaults', {
        method: 'POST',
        headers: { 'Content-Type': 'application/json' },
        body: JSON.stringify({ name: 'main' }),
      }),
      env,
    )

    await internal.putFile(
      createRequest('https://example.com', {
        method: 'PUT',
        headers: { 'X-Content-Hash': 'hash-1' },
        body: new TextEncoder().encode('first'),
      }),
      env,
      vault.vault.id,
      'note.md',
    )

    const response = await internal.batch(
      createRequest('https://example.com', {
        method: 'POST',
        headers: { 'Content-Type': 'application/json' },
        body: JSON.stringify({
          operations: [
            {
              action: 'put',
              path: 'note.md',
              parentHash: 'stale',
              contentHash: 'hash-2',
              content: btoa('second'),
            },
            {
              action: 'put',
              path: 'fresh.md',
              contentHash: 'hash-3',
              content: btoa('fresh'),
            },
          ],
        }),
      }),
      env,
      vault.vault.id,
    )

    expect(response.status).toBe(200)
    const body = await response.json<{ results: Array<{ status: number }> }>()
    expect(body.results.map((result) => result.status)).toEqual([409, 200])
  })

  it('prunes versions beyond the newest ten per file', async () => {
    const env = createEnv()
    const bucket = env.FILES as unknown as FakeR2Bucket

    for (let index = 1; index <= 12; index += 1) {
      await bucket.put(`_versions/vault_1/note.md/${index}`, new TextEncoder().encode(String(index)))
    }

    await internal.pruneVersions(env, 100)

    const keys = bucket.keys()
    expect(keys).not.toContain('_versions/vault_1/note.md/1')
    expect(keys).not.toContain('_versions/vault_1/note.md/2')
    expect(keys).toContain('_versions/vault_1/note.md/12')
    expect(keys.filter((key) => key.startsWith('_versions/vault_1/note.md/'))).toHaveLength(10)
  })

  it('prunes versions older than the retention window', async () => {
    const env = createEnv()
    const bucket = env.FILES as unknown as FakeR2Bucket

    await bucket.put('_versions/vault_1/old.md/1', new TextEncoder().encode('old'))
    await bucket.put('_versions/vault_1/old.md/1209601', new TextEncoder().encode('fresh'))

    await internal.pruneVersions(env, 14 * 24 * 60 * 60 + 5)

    const keys = bucket.keys()
    expect(keys).not.toContain('_versions/vault_1/old.md/1')
    expect(keys).toContain('_versions/vault_1/old.md/1209601')
  })

  it('prunes trash entries older than the retention window', async () => {
    const env = createEnv()
    const bucket = env.FILES as unknown as FakeR2Bucket

    await bucket.put('_trash/vault_1/note.md/1', new TextEncoder().encode('old'))
    await bucket.put('_trash/vault_1/note.md/2592001', new TextEncoder().encode('fresh'))

    await internal.pruneTrash(env, 30 * 24 * 60 * 60 + 5)

    const keys = bucket.keys()
    expect(keys).not.toContain('_trash/vault_1/note.md/1')
    expect(keys).toContain('_trash/vault_1/note.md/2592001')
  })
})
