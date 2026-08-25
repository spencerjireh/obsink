import {
  AuthError,
  OPERATOR_TENANT,
  appleSignIn,
  deleteAccount,
  emailStart,
  emailVerify,
  maxVaultsPerUser,
  me,
  resolvePrincipal,
  revokeSession,
  type AuthEnv,
  type Principal,
} from './auth'

export interface Env extends AuthEnv {
  META: KVNamespace
  FILES: R2Bucket
  MAX_BATCH_INLINE_BYTES?: string
  /** Per-vault byte budget for account (non-operator) vaults. Default 1 GiB. */
  MAX_VAULT_BYTES?: string
}

export interface FileEntry {
  hash: string
  modified: number
  size: number
  deleted?: boolean
  /** AES-GCM-encrypted real path (base64), supplied by the client on upload so
   * a fresh device can recover filenames from the token-keyed manifest. */
  encPath?: string
}

export type Manifest = Record<string, FileEntry>

export interface VaultSummary {
  id: string
  name: string
  created: number
  max_file_size: number
}

interface CreateVaultRequest {
  name?: string
  max_file_size?: number
}

interface BatchRequest {
  operations?: BatchOperation[]
}

type BatchOperation =
  | {
      action: 'put'
      path: string
      parentHash?: string
      contentHash: string
      content: string
      encPath?: string
    }
  | {
      action: 'delete'
      path: string
      parentHash?: string
    }

interface BatchOperationResult {
  path: string
  status: number
  conflict: { path: string; current: FileEntry | null } | null
}

const DEFAULT_MAX_FILE_SIZE = 50 * 1024 * 1024
const DEFAULT_MAX_VAULT_BYTES = 1024 * 1024 * 1024
const MANIFEST_PREFIX = 'manifest:'
/** The operator's (self-hosting) vault list keeps the pre-accounts key so
 * existing deployments need no migration; account vault lists are per user. */
const VAULTS_KEY = 'vaults'
const VERSION_RETENTION_SECS = 14 * 24 * 60 * 60
const TRASH_RETENTION_SECS = 30 * 24 * 60 * 60
const MAX_VERSIONS_PER_FILE = 10

export default {
  async fetch(request, env, ctx): Promise<Response> {
    const url = new URL(request.url)
    const path = trimPath(url.pathname)

    try {
      // Unauthenticated: capabilities + sign-in.
      if (request.method === 'GET' && path === '') {
        return json(capabilities(env))
      }
      if (request.method === 'POST' && path === 'auth/email/start') {
        const body = await readJson(request)
        return json(await emailStart(env, body.email))
      }
      if (request.method === 'POST' && path === 'auth/email/verify') {
        const body = await readJson(request)
        return json(await emailVerify(env, body.email, body.code, body.device_name))
      }
      if (request.method === 'POST' && path === 'auth/apple') {
        const body = await readJson(request)
        return json(await appleSignIn(env, body.identity_token, body.device_name, body.email))
      }

      const principal = await resolvePrincipal(request, env)
      if (!principal) {
        return json({ error: 'unauthorized' }, 401)
      }
      const tenant = principal.tenant

      if (request.method === 'GET' && path === 'auth/me') {
        return json(await me(env, principal))
      }
      if (request.method === 'DELETE' && path === 'auth/session') {
        await revokeSession(env, principal)
        return new Response(null, { status: 204 })
      }
      if (request.method === 'DELETE' && path.startsWith('auth/sessions/')) {
        await revokeSession(env, principal, path.slice('auth/sessions/'.length))
        return new Response(null, { status: 204 })
      }
      if (request.method === 'DELETE' && path === 'auth/account') {
        for (const vault of await listVaults(env, tenant)) {
          await deleteVaultData(env, vault.id)
        }
        await env.META.delete(vaultsKey(tenant))
        await deleteAccount(env, principal)
        return new Response(null, { status: 204 })
      }

      if (request.method === 'GET' && path === 'vaults') {
        return json(await listVaults(env, tenant))
      }

      if (request.method === 'POST' && path === 'vaults') {
        return json(await createVault(request, env, tenant), 201)
      }

      const route = parseVaultRoute(path)
      if (!route) {
        return json({ error: 'not_found' }, 404)
      }

      if (request.method === 'DELETE' && route.kind === 'vault') {
        await deleteVault(env, route.vaultId, tenant)
        return new Response(null, { status: 204 })
      }

      if (request.method === 'GET' && route.kind === 'manifest') {
        return json(await getManifest(env, route.vaultId, tenant))
      }

      if (request.method === 'GET' && route.kind === 'file') {
        return await getFile(env, route.vaultId, route.filePath, tenant)
      }

      if (request.method === 'PUT' && route.kind === 'file') {
        return await putFile(request, env, route.vaultId, route.filePath, tenant)
      }

      if (request.method === 'DELETE' && route.kind === 'file') {
        return await deleteFile(request, env, route.vaultId, route.filePath, tenant)
      }

      if (request.method === 'POST' && route.kind === 'batch') {
        return await batch(request, env, route.vaultId, tenant)
      }

      return json({ error: 'not_found' }, 404)
    } catch (error) {
      ctx.waitUntil(Promise.resolve())
      return handleError(error)
    }
  },

  async scheduled(controller, env, ctx): Promise<void> {
    ctx.waitUntil(pruneVersions(env, nowSeconds()))
    ctx.waitUntil(pruneTrash(env, nowSeconds()))
  },
} satisfies ExportedHandler<Env>

/** What sign-in methods this deployment offers; clients read it before
 * showing the account UI. Public by design (no secrets, no state). */
function capabilities(env: Env): { service: string; auth: { email: boolean; apple: boolean; api_key: boolean } } {
  return {
    service: 'obsink',
    auth: {
      email: Boolean(env.RESEND_API_KEY) || env.AUTH_DEV_RETURN_CODE === '1',
      apple: Boolean(env.APPLE_CLIENT_IDS),
      api_key: Boolean(env.API_KEY),
    },
  }
}

async function readJson(request: Request): Promise<Record<string, unknown>> {
  try {
    const body = (await request.json()) as unknown
    return body && typeof body === 'object' ? (body as Record<string, unknown>) : {}
  } catch {
    throw new HttpError(400, 'body must be JSON')
  }
}

function vaultsKey(tenant: string): string {
  return tenant === OPERATOR_TENANT ? VAULTS_KEY : `${VAULTS_KEY}:${tenant}`
}

function trimPath(pathname: string): string {
  return pathname.replace(/^\/+|\/+$/g, '')
}

function parseVaultRoute(path: string):
  | { vaultId: string; kind: 'vault' }
  | { vaultId: string; kind: 'manifest' }
  | { vaultId: string; kind: 'batch' }
  | { vaultId: string; kind: 'file'; filePath: string }
  | null {
  const parts = path.split('/')
  if (parts[0] !== 'vaults' || !parts[1]) {
    return null
  }

  if (parts.length === 2) {
    return { vaultId: parts[1], kind: 'vault' }
  }

  if (parts[2] === 'manifest' && parts.length === 3) {
    return { vaultId: parts[1], kind: 'manifest' }
  }

  if (parts[2] === 'batch' && parts.length === 3) {
    return { vaultId: parts[1], kind: 'batch' }
  }

  if (parts[2] === 'files' && parts.length >= 4) {
    return {
      vaultId: parts[1],
      kind: 'file',
      filePath: decodeURIComponent(parts.slice(3).join('/')),
    }
  }

  return null
}

async function listVaults(env: Env, tenant: string = OPERATOR_TENANT): Promise<VaultSummary[]> {
  return (await env.META.get(vaultsKey(tenant), 'json')) ?? []
}

async function createVault(
  request: Request,
  env: Env,
  tenant: string = OPERATOR_TENANT,
): Promise<{ vault: VaultSummary }> {
  const body = (await request.json()) as CreateVaultRequest
  if (!body.name?.trim()) {
    throw new HttpError(400, 'vault name is required')
  }

  const vaults = await listVaults(env, tenant)
  if (tenant !== OPERATOR_TENANT && vaults.length >= maxVaultsPerUser(env)) {
    throw new HttpError(403, `vault limit reached (${maxVaultsPerUser(env)} per account)`)
  }
  const vault: VaultSummary = {
    id: `vault_${crypto.randomUUID()}`,
    name: body.name.trim(),
    created: nowSeconds(),
    max_file_size: Math.min(body.max_file_size ?? DEFAULT_MAX_FILE_SIZE, DEFAULT_MAX_FILE_SIZE),
  }

  vaults.push(vault)
  await env.META.put(vaultsKey(tenant), JSON.stringify(vaults))
  await writeManifest(env, vault.id, {})
  return { vault }
}

/** Remove a vault from its owner's list and delete every blob, version, and
 * trash entry it owns. Irreversible; the client confirms before calling. */
async function deleteVault(env: Env, vaultId: string, tenant: string = OPERATOR_TENANT): Promise<void> {
  await requireVault(env, vaultId, tenant)
  await deleteVaultData(env, vaultId)
  const remaining = (await listVaults(env, tenant)).filter((vault) => vault.id !== vaultId)
  await env.META.put(vaultsKey(tenant), JSON.stringify(remaining))
}

async function deleteVaultData(env: Env, vaultId: string): Promise<void> {
  for (const prefix of [`${vaultId}/`, `_versions/${vaultId}/`, `_trash/${vaultId}/`]) {
    let cursor: string | undefined
    do {
      const page = await env.FILES.list({ prefix, cursor })
      await Promise.all(page.objects.map((object) => env.FILES.delete(object.key)))
      cursor = page.truncated ? page.cursor : undefined
    } while (cursor)
  }
  await env.META.delete(`${MANIFEST_PREFIX}${vaultId}`)
}

async function getManifest(env: Env, vaultId: string, tenant: string = OPERATOR_TENANT): Promise<Manifest> {
  await requireVault(env, vaultId, tenant)
  return readManifest(env, vaultId)
}

async function getFile(
  env: Env,
  vaultId: string,
  filePath: string,
  tenant: string = OPERATOR_TENANT,
): Promise<Response> {
  await requireVault(env, vaultId, tenant)
  const object = await env.FILES.get(fileObjectKey(vaultId, filePath))
  if (!object) {
    return json({ error: 'not_found' }, 404)
  }

  return new Response(object.body, {
    headers: {
      'Content-Type': 'application/octet-stream',
      'Cache-Control': 'no-store',
    },
  })
}

async function putFile(
  request: Request,
  env: Env,
  vaultId: string,
  filePath: string,
  tenant: string = OPERATOR_TENANT,
): Promise<Response> {
  const vault = await requireVault(env, vaultId, tenant)
  const manifest = await readManifest(env, vaultId)
  const current = manifest[filePath]
  const parentHash = request.headers.get('X-Parent-Hash')
  const contentHash = request.headers.get('X-Content-Hash')
  const encPath = request.headers.get('X-Enc-Path') ?? current?.encPath ?? ''

  if (!contentHash) {
    throw new HttpError(400, 'missing X-Content-Hash header')
  }

  const body = new Uint8Array(await request.arrayBuffer())
  if (body.byteLength > vault.max_file_size) {
    throw new HttpError(413, 'file too large')
  }
  if (tenant !== OPERATOR_TENANT) {
    const used = Object.entries(manifest)
      .filter(([path, entry]) => !entry.deleted && path !== filePath)
      .reduce((total, [, entry]) => total + entry.size, 0)
    if (used + body.byteLength > maxVaultBytes(env)) {
      throw new HttpError(413, 'vault storage limit reached')
    }
  }

  if (current && current.hash !== parentHash) {
    return json({ path: filePath, current }, 409)
  }

  if (current && !current.deleted) {
    await archiveVersion(env, vaultId, filePath)
  }

  await env.FILES.put(fileObjectKey(vaultId, filePath), body)
  manifest[filePath] = {
    hash: contentHash,
    modified: nowSeconds(),
    size: body.byteLength,
    deleted: false,
    encPath,
  }
  await writeManifest(env, vaultId, manifest)

  return new Response(null, { status: 200 })
}

async function deleteFile(
  request: Request,
  env: Env,
  vaultId: string,
  filePath: string,
  tenant: string = OPERATOR_TENANT,
): Promise<Response> {
  await requireVault(env, vaultId, tenant)
  const manifest = await readManifest(env, vaultId)
  const current = manifest[filePath]
  const parentHash = request.headers.get('X-Parent-Hash')

  if (current && current.hash !== parentHash) {
    return json({ path: filePath, current }, 409)
  }

  const objectKey = fileObjectKey(vaultId, filePath)
  const object = await env.FILES.get(objectKey)
  if (object) {
    await env.FILES.put(trashObjectKey(vaultId, filePath, nowSeconds()), object.body)
    await env.FILES.delete(objectKey)
  }

  manifest[filePath] = {
    hash: current?.hash ?? '',
    modified: nowSeconds(),
    size: current?.size ?? 0,
    deleted: true,
    encPath: current?.encPath ?? '',
  }
  await writeManifest(env, vaultId, manifest)

  return new Response(null, { status: 200 })
}

async function batch(
  request: Request,
  env: Env,
  vaultId: string,
  tenant: string = OPERATOR_TENANT,
): Promise<Response> {
  const body = (await request.json()) as BatchRequest
  if (!Array.isArray(body.operations)) {
    throw new HttpError(400, 'operations must be an array')
  }

  const maxInlineBytes = Number(env.MAX_BATCH_INLINE_BYTES ?? DEFAULT_MAX_FILE_SIZE)
  const results: BatchOperationResult[] = []

  for (const operation of body.operations) {
    try {
      if (operation.action === 'put') {
        const content = Uint8Array.from(atob(operation.content), (char) => char.charCodeAt(0))
        if (content.byteLength > maxInlineBytes) {
          throw new HttpError(413, 'batch inline content exceeds configured limit')
        }

        const response = await putFile(
          new Request(`https://worker.invalid/vaults/${vaultId}/files/${encodeURIComponent(operation.path)}`, {
            method: 'PUT',
            headers: {
              'X-Parent-Hash': operation.parentHash ?? '',
              'X-Content-Hash': operation.contentHash,
              'X-Enc-Path': operation.encPath ?? '',
            },
            body: content,
          }),
          env,
          vaultId,
          operation.path,
          tenant,
        )

        if (response.status === 409) {
          results.push({
            path: operation.path,
            status: 409,
            conflict: (await response.json()) as { path: string; current: FileEntry | null },
          })
        } else {
          results.push({ path: operation.path, status: response.status, conflict: null })
        }
      } else {
        const response = await deleteFile(
          new Request(`https://worker.invalid/vaults/${vaultId}/files/${encodeURIComponent(operation.path)}`, {
            method: 'DELETE',
            headers: {
              'X-Parent-Hash': operation.parentHash ?? '',
            },
          }),
          env,
          vaultId,
          operation.path,
          tenant,
        )

        if (response.status === 409) {
          results.push({
            path: operation.path,
            status: 409,
            conflict: (await response.json()) as { path: string; current: FileEntry | null },
          })
        } else {
          results.push({ path: operation.path, status: response.status, conflict: null })
        }
      }
    } catch (error) {
      if (error instanceof HttpError) {
        results.push({ path: operation.path, status: error.status, conflict: null })
        continue
      }
      throw error
    }
  }

  return json({ results })
}

async function requireVault(env: Env, vaultId: string, tenant: string = OPERATOR_TENANT): Promise<VaultSummary> {
  const vault = (await listVaults(env, tenant)).find((item) => item.id === vaultId)
  if (!vault) {
    throw new HttpError(404, 'vault not found')
  }
  return vault
}

async function readManifest(env: Env, vaultId: string): Promise<Manifest> {
  return (await env.META.get(`${MANIFEST_PREFIX}${vaultId}`, 'json')) ?? {}
}

async function writeManifest(env: Env, vaultId: string, manifest: Manifest): Promise<void> {
  await env.META.put(`${MANIFEST_PREFIX}${vaultId}`, JSON.stringify(manifest))
}

async function archiveVersion(env: Env, vaultId: string, filePath: string): Promise<void> {
  const current = await env.FILES.get(fileObjectKey(vaultId, filePath))
  if (!current) {
    return
  }

  await env.FILES.put(versionObjectKey(vaultId, filePath, nowSeconds()), current.body)
}

async function pruneVersions(env: Env, currentTime: number): Promise<void> {
  const grouped = new Map<string, R2Object[]>()
  let cursor: string | undefined

  do {
    const page = await env.FILES.list({ prefix: '_versions/', cursor })
    for (const object of page.objects) {
      const groupKey = object.key.split('/').slice(0, -1).join('/')
      const items = grouped.get(groupKey) ?? []
      items.push(object)
      grouped.set(groupKey, items)
    }
    cursor = page.truncated ? page.cursor : undefined
  } while (cursor)

  await Promise.all(
    Array.from(grouped.values()).flatMap((objects) => {
      objects.sort((a, b) => extractTimestamp(b.key) - extractTimestamp(a.key))
      return objects
        .filter((object, index) => {
          const age = currentTime - extractTimestamp(object.key)
          return index >= MAX_VERSIONS_PER_FILE || age > VERSION_RETENTION_SECS
        })
        .map((object) => env.FILES.delete(object.key))
    }),
  )
}

async function pruneTrash(env: Env, currentTime: number): Promise<void> {
  let cursor: string | undefined
  do {
    const page = await env.FILES.list({ prefix: '_trash/', cursor })
    await Promise.all(
      page.objects
        .filter((object) => currentTime - extractTimestamp(object.key) > TRASH_RETENTION_SECS)
        .map((object) => env.FILES.delete(object.key)),
    )
    cursor = page.truncated ? page.cursor : undefined
  } while (cursor)
}

function fileObjectKey(vaultId: string, filePath: string): string {
  return `${vaultId}/${filePath}`
}

function versionObjectKey(vaultId: string, filePath: string, timestamp: number): string {
  return `_versions/${vaultId}/${filePath}/${timestamp}`
}

function trashObjectKey(vaultId: string, filePath: string, timestamp: number): string {
  return `_trash/${vaultId}/${filePath}/${timestamp}`
}

function extractTimestamp(key: string): number {
  const value = Number(key.split('/').at(-1))
  return Number.isFinite(value) ? value : 0
}

function maxVaultBytes(env: Env): number {
  const value = Number(env.MAX_VAULT_BYTES ?? DEFAULT_MAX_VAULT_BYTES)
  return Number.isFinite(value) && value > 0 ? value : DEFAULT_MAX_VAULT_BYTES
}

function nowSeconds(): number {
  return Math.floor(Date.now() / 1000)
}

function json(data: unknown, status = 200): Response {
  return new Response(JSON.stringify(data), {
    status,
    headers: { 'Content-Type': 'application/json' },
  })
}

function handleError(error: unknown): Response {
  if (error instanceof HttpError || error instanceof AuthError) {
    return json({ error: error.message }, error.status)
  }

  const message = error instanceof Error ? error.message : 'internal server error'
  return json({ error: message }, 500)
}

class HttpError extends Error {
  constructor(
    public readonly status: number,
    message: string,
  ) {
    super(message)
  }
}

export const internal = {
  batch,
  createVault,
  deleteFile,
  getFile,
  deleteVault,
  getManifest,
  listVaults,
  parseVaultRoute,
  pruneTrash,
  pruneVersions,
  putFile,
  readManifest,
  requireVault,
  writeManifest,
}
