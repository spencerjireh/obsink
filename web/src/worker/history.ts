import type { FilePreview, TrashEntry, VersionInfo } from '@obsink/ui'
import { other } from './errors'
import { scan, writeFile } from './fs'
import { keysFor } from './keys'
import { ignoreFor } from './manifest'
import { api, bearerCall } from './session'
import { handleFor, vaultById } from './vaults'
import type { VaultKeys } from './wasm'

// Spec §8.2 and §9.3 in the browser: the read routes decrypted with the
// vault's keys, and restore as a plain write into the folder (the next sync
// uploads it through the ordinary conflict-gated path; a restored deletion
// carries the tombstone as its parent because the base still holds it).

async function open(
  vaultId: string,
): Promise<{ keys: VaultKeys; root: FileSystemDirectoryHandle }> {
  const vault = await vaultById(vaultId)
  const keys = keysFor(vaultId)
  if (!keys) throw other('Unlock the account first.')
  return { keys, root: await handleFor(vault) }
}

// UTF-8 text or nothing: a decoder that refuses invalid bytes tells the two
// apart without guessing from the extension.
function preview(bytes: Uint8Array): FilePreview {
  try {
    const text = new TextDecoder('utf-8', { fatal: true }).decode(bytes)
    return { text: text.includes('\0') ? null : text, size: bytes.length }
  } catch {
    return { text: null, size: bytes.length }
  }
}

export async function listFiles(vaultId: string): Promise<string[]> {
  const vault = await vaultById(vaultId)
  const root = await handleFor(vault)
  const files = await scan(root, await ignoreFor(vault))
  return files.map((file) => file.path)
}

export async function listVersions(vaultId: string, path: string): Promise<VersionInfo[]> {
  const { keys } = await open(vaultId)
  return bearerCall((bearer) => api.listVersions(bearer, vaultId, keys.pathToken(path)))
}

async function versionBytes(vaultId: string, path: string, name: string) {
  const { keys, root } = await open(vaultId)
  const blob = await bearerCall((bearer) =>
    api.getVersion(bearer, vaultId, name, keys.pathToken(path)),
  )
  return { root, bytes: keys.decrypt(blob) }
}

export async function previewVersion(
  vaultId: string,
  path: string,
  name: string,
): Promise<FilePreview> {
  return preview((await versionBytes(vaultId, path, name)).bytes)
}

export async function restoreVersion(vaultId: string, path: string, name: string): Promise<void> {
  const { root, bytes } = await versionBytes(vaultId, path, name)
  await writeFile(root, path, bytes)
}

export async function listTrash(vaultId: string): Promise<TrashEntry[]> {
  const { keys } = await open(vaultId)
  const entries = await bearerCall((bearer) => api.listTrash(bearer, vaultId))
  return entries.map((entry) => ({
    path: keys.decryptPath(entry.encPath),
    hash: entry.hash,
    size: entry.size,
    deleted_at: entry.deleted_at,
  }))
}

async function trashBytes(vaultId: string, path: string) {
  const { keys, root } = await open(vaultId)
  const blob = await bearerCall((bearer) => api.getTrash(bearer, vaultId, keys.pathToken(path)))
  return { root, bytes: keys.decrypt(blob) }
}

export async function previewTrash(vaultId: string, path: string): Promise<FilePreview> {
  return preview((await trashBytes(vaultId, path)).bytes)
}

export async function restoreTrash(vaultId: string, path: string): Promise<void> {
  const { root, bytes } = await trashBytes(vaultId, path)
  await writeFile(root, path, bytes)
}
