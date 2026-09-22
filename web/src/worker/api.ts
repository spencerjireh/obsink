import { fromStatus, network } from './errors'

// The server's HTTP surface as the browser sees it: same-origin paths (the
// web container proxies them to the API), a bearer per call, `{ "error" }`
// bodies turned into typed errors, and the same transient-failure retry as
// core's `api_client` (three attempts, 100 ms * 2^n).

const SMALL_TIMEOUT_MS = 30_000
const BATCH_TIMEOUT_MS = 10 * 60_000
const ATTEMPTS = 3
// Only these are repeated after a request that got no answer: a POST the
// server may already have applied (a batch, a new vault, a sign-in) is
// reported instead of replayed.
const IDEMPOTENT = new Set(['GET', 'HEAD', 'DELETE'])

export type WireFileEntry = {
  hash: string
  modified: number
  size: number
  deleted?: boolean
  encPath?: string
}

export type Capabilities = {
  service: string
  protocol?: number
  auth: { email: boolean; apple: boolean }
  invite_required?: boolean
}

export type Session = {
  token: string
  session: { id: string; expires: number; device_id?: string }
  user: { id: string; email: string | null }
}

export type WireDevice = {
  id: string
  name: string
  platform: string
  created: number
  last_seen?: number
  current: boolean
  vault_ids?: string[]
}

export type Me = {
  user: { id: string; email: string | null; created: number } | null
  devices?: WireDevice[]
  usage?: {
    vaults?: { id: string; bytes: number }[]
    total_bytes: number
    max_vault_bytes: number | null
    max_vaults: number | null
  } | null
}

// The wrapped account key as the server holds it (spec §6.1).
export type AccountKeyBlob = { key_id: string; wrapped: string; salt: string }
export type AccountKeyMaterial = { wrapped: string; salt: string; verifier: string }
export type SetKeysOutcome =
  { outcome: 'created'; key_id: string } | { outcome: 'exists'; blob: AccountKeyBlob }

export type WireInvite = {
  code: string
  created?: number
  expires: number
  status?: string
  used_at?: number | null
}

export type VaultSummaryDevice = {
  id: string
  name: string
  platform: string
  last_synced?: number | null
  last_revision?: number | null
}

export type VaultSummary = {
  id: string
  name: string
  created: number
  max_file_size?: number
  revision?: number
  last_write?: number
  bytes?: number
  wrapped_key?: string | null
  devices?: VaultSummaryDevice[]
}

// Spec §8.2 and §9.3 read routes; `path` in the trash entry is the token.
export type WireVersion = { name: string; ts: number; size: number }
export type WireTrashEntry = {
  path: string
  encPath: string
  hash: string
  size: number
  deleted_at: number
}

export type ManifestFetch =
  | { status: 'not_modified' }
  | { status: 'ok'; etag: string | null; manifest: Record<string, WireFileEntry> }

export type BatchOperation =
  | { action: 'put'; path: string; parentHash?: string; contentHash: string; encPath: string }
  | { action: 'delete'; path: string; parentHash?: string }

export type BatchResult = {
  path: string
  status: number
  conflict: { path: string; current: WireFileEntry | null } | null
}

type Options = {
  bearer?: string
  json?: unknown
  body?: BodyInit
  headers?: Record<string, string>
  cache?: RequestCache
  timeoutMs?: number
  // Statuses the caller handles itself (returned instead of thrown).
  accept?: number[]
}

function errorMessage(body: string, status: number): string {
  try {
    const parsed = JSON.parse(body) as { error?: unknown }
    if (typeof parsed.error === 'string') return parsed.error
  } catch {
    // not JSON
  }
  return body || `server returned ${status}`
}

function sleep(ms: number): Promise<void> {
  return new Promise((resolve) => setTimeout(resolve, ms))
}

export class Api {
  constructor(private base: string) {}

  private async send(method: string, path: string, options: Options): Promise<Response> {
    const headers = new Headers(options.headers)
    if (options.bearer) headers.set('Authorization', `Bearer ${options.bearer}`)
    let body: BodyInit | undefined = options.body
    if (options.json !== undefined) {
      headers.set('Content-Type', 'application/json')
      body = JSON.stringify(options.json)
    }
    let lastError: unknown = null
    for (let attempt = 0; attempt < ATTEMPTS; attempt++) {
      const controller = new AbortController()
      const timer = setTimeout(() => controller.abort(), options.timeoutMs ?? SMALL_TIMEOUT_MS)
      try {
        const response = await fetch(`${this.base}${path}`, {
          method,
          headers,
          body,
          signal: controller.signal,
          credentials: 'omit',
          cache: options.cache,
        })
        clearTimeout(timer)
        if (response.ok || options.accept?.includes(response.status)) return response
        throw fromStatus(response.status, errorMessage(await response.text(), response.status))
      } catch (error) {
        clearTimeout(timer)
        // Only a request that never got an answer is retried.
        const transient = error instanceof TypeError || (error as Error)?.name === 'AbortError'
        if (!transient) throw error
        lastError = error
        if (!IDEMPOTENT.has(method)) break
        if (attempt + 1 < ATTEMPTS) await sleep(100 * 2 ** attempt)
      }
    }
    throw network(lastError instanceof Error ? lastError.message : 'could not reach the server')
  }

  private async json<T>(method: string, path: string, options: Options = {}): Promise<T> {
    const response = await this.send(method, path, options)
    const text = await response.text()
    return (text ? JSON.parse(text) : {}) as T
  }

  capabilities(): Promise<Capabilities> {
    // Fetch's default Accept is */*, which the proxy routes to the API. The
    // same URL serves the landing page to navigations, so bypass the HTTP
    // cache: a cached HTML response must never answer this probe.
    return this.json('GET', '/', { headers: { Accept: 'application/json' }, cache: 'no-store' })
  }

  emailStart(email: string): Promise<{ sent: boolean; code?: string | null }> {
    return this.json('POST', '/auth/email/start', { json: { email } })
  }

  // v2 device identity (a name only) until OBS-141 gives the browser a
  // stable device id.
  emailVerify(
    email: string,
    code: string,
    deviceName: string,
    inviteCode: string | null,
  ): Promise<Session> {
    return this.json('POST', '/auth/email/verify', {
      json: { email, code, device_name: deviceName, invite_code: inviteCode },
    })
  }

  me(bearer: string): Promise<Me> {
    return this.json('GET', '/auth/me', { bearer })
  }

  async getKeys(bearer: string): Promise<AccountKeyBlob | null> {
    const { account_key } = await this.json<{ account_key: AccountKeyBlob | null }>(
      'GET',
      '/auth/keys',
      { bearer },
    )
    return account_key
  }

  // Create-only: a 409 carries the blob another device set first.
  async setKeys(bearer: string, material: AccountKeyMaterial): Promise<SetKeysOutcome> {
    const response = await this.send('PUT', '/auth/keys', {
      bearer,
      json: material,
      accept: [409],
    })
    if (response.status === 409) {
      const { account_key } = (await response.json()) as { account_key: AccountKeyBlob | null }
      if (!account_key) throw fromStatus(409, 'the passphrase was set elsewhere; sign in again')
      return { outcome: 'exists', blob: account_key }
    }
    const { key_id } = (await response.json()) as { key_id: string }
    return { outcome: 'created', key_id }
  }

  async rewrapKeys(bearer: string, material: AccountKeyMaterial): Promise<void> {
    await this.send('PUT', '/auth/keys/rewrap', { bearer, json: material })
  }

  async logout(bearer: string): Promise<void> {
    await this.send('DELETE', '/auth/session', { bearer })
  }

  async revokeDevice(bearer: string, deviceId: string): Promise<void> {
    await this.send('DELETE', `/auth/devices/${encodeURIComponent(deviceId)}`, { bearer })
  }

  async renameDevice(bearer: string, deviceId: string, name: string): Promise<void> {
    await this.send('PATCH', `/auth/devices/${encodeURIComponent(deviceId)}`, {
      bearer,
      json: { name },
    })
  }

  async deleteAccount(bearer: string): Promise<void> {
    await this.send('DELETE', '/auth/account', { bearer })
  }

  async createInvite(bearer: string): Promise<WireInvite> {
    const { invite } = await this.json<{ invite: WireInvite }>('POST', '/auth/invites', { bearer })
    return invite
  }

  async listInvites(bearer: string): Promise<WireInvite[]> {
    const { invites } = await this.json<{ invites: WireInvite[] }>('GET', '/auth/invites', {
      bearer,
    })
    return invites
  }

  async listVaults(bearer: string): Promise<VaultSummary[]> {
    const { vaults } = await this.json<{ vaults: VaultSummary[] }>('GET', '/vaults', { bearer })
    return vaults
  }

  // The client mints the id so the vault key is wrapped under it (spec §4.3).
  async createVault(
    bearer: string,
    request: { id: string; name: string; wrapped_key: string; max_file_size: number },
  ): Promise<VaultSummary> {
    const { vault } = await this.json<{ vault: VaultSummary }>('POST', '/vaults', {
      bearer,
      json: request,
    })
    return vault
  }

  async renameVault(bearer: string, vaultId: string, name: string): Promise<void> {
    await this.send('PATCH', `/vaults/${vaultId}`, { bearer, json: { name } })
  }

  async deleteVault(bearer: string, vaultId: string): Promise<void> {
    await this.send('DELETE', `/vaults/${vaultId}`, { bearer })
  }

  // This device holds the vault (no revision) or just synced it to `revision`.
  async attachDevice(bearer: string, vaultId: string, revision: number | null): Promise<void> {
    await this.send('PUT', `/vaults/${vaultId}/devices/self`, {
      bearer,
      json: revision === null ? {} : { revision },
    })
  }

  async detachDevice(bearer: string, vaultId: string): Promise<void> {
    await this.send('DELETE', `/vaults/${vaultId}/devices/self`, { bearer })
  }

  async getManifest(
    bearer: string,
    vaultId: string,
    ifNoneMatch: string | null,
  ): Promise<ManifestFetch> {
    const headers: Record<string, string> = {}
    if (ifNoneMatch) headers['If-None-Match'] = ifNoneMatch
    const response = await this.send('GET', `/vaults/${vaultId}/manifest`, {
      bearer,
      headers,
      accept: [304],
    })
    if (response.status === 304) return { status: 'not_modified' }
    return {
      status: 'ok',
      etag: response.headers.get('ETag'),
      manifest: (await response.json()) as Record<string, WireFileEntry>,
    }
  }

  async getFile(bearer: string, vaultId: string, token: string): Promise<Uint8Array> {
    const response = await this.send('GET', `/vaults/${vaultId}/files/${token}`, { bearer })
    return new Uint8Array(await response.arrayBuffer())
  }

  async listVersions(bearer: string, vaultId: string, token: string): Promise<WireVersion[]> {
    const { versions } = await this.json<{ versions: WireVersion[] }>(
      'GET',
      `/vaults/${vaultId}/history/${token}`,
      { bearer },
    )
    return versions
  }

  async getVersion(
    bearer: string,
    vaultId: string,
    name: string,
    token: string,
  ): Promise<Uint8Array> {
    const response = await this.send('GET', `/vaults/${vaultId}/versions/${name}/${token}`, {
      bearer,
    })
    return new Uint8Array(await response.arrayBuffer())
  }

  async listTrash(bearer: string, vaultId: string): Promise<WireTrashEntry[]> {
    const { entries } = await this.json<{ entries: WireTrashEntry[] }>(
      'GET',
      `/vaults/${vaultId}/trash`,
      { bearer },
    )
    return entries
  }

  async getTrash(bearer: string, vaultId: string, token: string): Promise<Uint8Array> {
    const response = await this.send('GET', `/vaults/${vaultId}/trash/${token}`, { bearer })
    return new Uint8Array(await response.arrayBuffer())
  }

  // One multipart request: an `operations` JSON part plus one `content`
  // part per put, named by the operation's index (core `api_client::batch`).
  async batch(
    bearer: string,
    vaultId: string,
    operations: BatchOperation[],
    contents: Map<number, Uint8Array>,
  ): Promise<BatchResult[]> {
    const form = new FormData()
    form.append(
      'operations',
      new Blob([JSON.stringify({ operations })], { type: 'application/json' }),
    )
    for (const [index, bytes] of contents) {
      form.append(
        'content',
        new Blob([bytes as BlobPart], { type: 'application/octet-stream' }),
        String(index),
      )
    }
    const { results } = await this.json<{ results: BatchResult[] }>(
      'POST',
      `/vaults/${vaultId}/batch`,
      { bearer, body: form, timeoutMs: BATCH_TIMEOUT_MS },
    )
    if (results.length !== operations.length) {
      throw fromStatus(
        502,
        `batch answered ${results.length} results for ${operations.length} operations`,
      )
    }
    return results
  }
}
