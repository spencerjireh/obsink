export interface Env {
  META: KVNamespace
  FILES: R2Bucket
  API_KEY: string
  MAX_BATCH_INLINE_BYTES?: string
}

export interface FileEntry {
  hash: string
  modified: number
  size: number
  deleted?: boolean
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
const MANIFEST_PREFIX = 'manifest:'
const VAULTS_KEY = 'vaults'
const VERSION_RETENTION_SECS = 14 * 24 * 60 * 60
const TRASH_RETENTION_SECS = 30 * 24 * 60 * 60
const MAX_VERSIONS_PER_FILE = 10

export default {
  async fetch(request, env, ctx): Promise<Response> {
    if (!isAuthorized(request, env)) {
      return json({ error: 'unauthorized' }, 401)
    }

    const url = new URL(request.url)
    const path = trimPath(url.pathname)

    try {
      if (request.method === 'GET' && path === 'vaults') {
        return json(await listVaults(env))
      }

      if (request.method === 'POST' && path === 'vaults') {
        return json(await createVault(request, env), 201)
      }

      const route = parseVaultRoute(path)
      if (!route) {
        return json({ error: 'not_found' }, 404)
      }

      if (request.method === 'GET' && route.kind === 'manifest') {
        return json(await getManifest(env, route.vaultId))
      }

      if (request.method === 'GET' && route.kind === 'file') {
        return await getFile(env, route.vaultId, route.filePath)
      }

      if (request.method === 'PUT' && route.kind === 'file') {
        return await putFile(request, env, route.vaultId, route.filePath)
      }

      if (request.method === 'DELETE' && route.kind === 'file') {
        return await deleteFile(request, env, route.vaultId, route.filePath)
      }

      if (request.method === 'POST' && route.kind === 'batch') {
        return await batch(request, env, route.vaultId)
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

function isAuthorized(request: Request, env: Env): boolean {
  const auth = request.headers.get('Authorization')
  return auth === `Bearer ${env.API_KEY}`
}

function trimPath(pathname: string): string {
  return pathname.replace(/^\/+|\/+$/g, '')
}

function parseVaultRoute(path: string):
  | { vaultId: string; kind: 'manifest' }
  | { vaultId: string; kind: 'batch' }
  | { vaultId: string; kind: 'file'; filePath: string }
  | null {
  const parts = path.split('/')
  if (parts[0] !== 'vaults' || !parts[1]) {
    return null
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

async function listVaults(env: Env): Promise<VaultSummary[]> {
  return (await env.META.get(VAULTS_KEY, 'json')) ?? []
}

async function createVault(request: Request, env: Env): Promise<{ vault: VaultSummary }> {
  const body = (await request.json()) as CreateVaultRequest
  if (!body.name?.trim()) {
    throw new HttpError(400, 'vault name is required')
  }

  const vaults = await listVaults(env)
  const vault: VaultSummary = {
    id: `vault_${crypto.randomUUID()}`,
    name: body.name.trim(),
    created: nowSeconds(),
    max_file_size: body.max_file_size ?? DEFAULT_MAX_FILE_SIZE,
  }

  vaults.push(vault)
  await env.META.put(VAULTS_KEY, JSON.stringify(vaults))
  await writeManifest(env, vault.id, {})
  return { vault }
}

async function getManifest(env: Env, vaultId: string): Promise<Manifest> {
  await requireVault(env, vaultId)
  return readManifest(env, vaultId)
}

async function getFile(env: Env, vaultId: string, filePath: string): Promise<Response> {
  await requireVault(env, vaultId)
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
): Promise<Response> {
  const vault = await requireVault(env, vaultId)
  const manifest = await readManifest(env, vaultId)
  const current = manifest[filePath]
  const parentHash = request.headers.get('X-Parent-Hash')
  const contentHash = request.headers.get('X-Content-Hash')

  if (!contentHash) {
    throw new HttpError(400, 'missing X-Content-Hash header')
  }

  const body = new Uint8Array(await request.arrayBuffer())
  if (body.byteLength > vault.max_file_size) {
    throw new HttpError(413, 'file too large')
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
  }
  await writeManifest(env, vaultId, manifest)

  return new Response(null, { status: 200 })
}

async function deleteFile(
  request: Request,
  env: Env,
  vaultId: string,
  filePath: string,
): Promise<Response> {
  await requireVault(env, vaultId)
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
  }
  await writeManifest(env, vaultId, manifest)

  return new Response(null, { status: 200 })
}

async function batch(request: Request, env: Env, vaultId: string): Promise<Response> {
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
            },
            body: content,
          }),
          env,
          vaultId,
          operation.path,
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

async function requireVault(env: Env, vaultId: string): Promise<VaultSummary> {
  const vault = (await listVaults(env)).find((item) => item.id === vaultId)
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
  if (error instanceof HttpError) {
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
  getManifest,
  isAuthorized,
  listVaults,
  parseVaultRoute,
  pruneTrash,
  pruneVersions,
  putFile,
  readManifest,
  requireVault,
  writeManifest,
}
