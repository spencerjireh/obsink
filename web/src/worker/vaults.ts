import type {
  AddVaultRequest,
  CommandErrorKind,
  LocalVault,
  RemoteVault,
  VaultStateInfo,
} from '@obsink/ui'
import { all, del, get, KV_ACTIVE_VAULT, listVaults, put, type StoredVault } from '../shared/db'
import { other, wrongPassphrase } from './errors'
import { deriveKeys, forgetKeys, keysFor, rememberKeys } from './keys'
import {
  diffManifests,
  fetchRemoteManifest,
  loadLocalState,
  loadSyncState,
  saveSyncState,
} from './manifest'
import { api, bearerCall, serverUrl } from './session'
import { forgetActivity, lastSynced } from './activity'
import { inFlight, pendingConflicts, startDriver, stopDriver } from './driver'
import type { VaultKeys } from './wasm'

const MAX_FILE_SIZE = 50 * 1024 * 1024

function isErrorKind(kind: unknown): kind is CommandErrorKind {
  return kind === 'unauthorized' || kind === 'network' || kind === 'server' || kind === 'other'
}

export function listRemoteVaults(): Promise<RemoteVault[]> {
  return bearerCall(async (bearer) =>
    (await api.listVaults(bearer)).map((vault) => ({
      id: vault.id,
      name: vault.name,
      created: vault.created,
      max_file_size: vault.max_file_size ?? MAX_FILE_SIZE,
    })),
  )
}

export async function vaultById(vaultId: string): Promise<StoredVault> {
  const vault = await get<StoredVault>('vaults', vaultId)
  if (!vault) throw other('This vault is not configured in this browser. Add it again.')
  return vault
}

export async function handleFor(vault: StoredVault): Promise<FileSystemDirectoryHandle> {
  const handle = await get<FileSystemDirectoryHandle>('handles', vault.handle_id)
  if (!handle) throw other('Folder not found. Remove the vault and add it again.')
  return handle
}

function localVault(vault: StoredVault, active: boolean): LocalVault {
  return {
    id: vault.id,
    name: vault.name,
    server_url: vault.server_url,
    local_path: vault.folder_name,
    active,
  }
}

// The passphrase is right when the first live file decrypts (an empty
// vault accepts any passphrase, as on desktop).
async function validatePassphrase(vault: StoredVault, keys: VaultKeys): Promise<void> {
  const state = await loadSyncState(vault.id)
  const manifest = await fetchRemoteManifest(vault, keys, state)
  const live = Object.entries(manifest).find(([, entry]) => !entry.deleted)
  if (live) {
    const [path] = live
    const blob = await bearerCall((bearer) => api.getFile(bearer, vault.id, keys.pathToken(path)))
    try {
      keys.decrypt(blob)
    } catch {
      throw wrongPassphrase()
    }
  }
  await saveSyncState(vault.id, state)
}

export async function addVault(request: AddVaultRequest): Promise<LocalVault> {
  if (!request.local_path.trim()) throw other('Choose a folder for the vault.')
  if (!request.passphrase) throw other('Enter a passphrase.')
  if (request.mode === 'create' && !request.vault_name.trim()) throw other('Enter a vault name.')
  if (request.mode === 'connect' && !request.vault_id.trim()) throw other('Enter a vault ID.')

  const handle = await get<FileSystemDirectoryHandle>('handles', request.local_path)
  if (!handle) throw other('Choose a folder for the vault.')

  const summary =
    request.mode === 'create'
      ? await bearerCall((bearer) =>
          api.createVault(bearer, request.vault_name.trim(), MAX_FILE_SIZE),
        )
      : await bearerCall(async (bearer) => {
          const found = (await api.listVaults(bearer)).find(
            (vault) => vault.id === request.vault_id,
          )
          if (!found) throw other(`Vault ${request.vault_id} was not found on the server.`)
          return found
        })

  const keys = await deriveKeys(summary.id, request.passphrase)
  const vault: StoredVault = {
    id: summary.id,
    name: summary.name,
    server_url: serverUrl,
    handle_id: request.local_path,
    folder_name: handle.name,
    ignore: [],
    created: Date.now(),
  }
  try {
    await validatePassphrase(vault, keys)
  } catch (error) {
    keys.free()
    throw error
  }
  rememberKeys(vault.id, keys)
  await put('vaults', vault.id, vault)
  await put('kv', KV_ACTIVE_VAULT, vault.id)
  await startDriver(vault.id)
  return localVault(vault, true)
}

// Forget a vault in this browser only: entry, handle, bookkeeping, log, key.
export async function forgetVault(vaultId: string): Promise<void> {
  const vault = await get<StoredVault>('vaults', vaultId)
  stopDriver(vaultId)
  forgetKeys(vaultId)
  await del('vaults', vaultId)
  if (vault) await del('handles', vault.handle_id)
  await del('state', vaultId)
  await forgetActivity(vaultId)
  if ((await get<string>('kv', KV_ACTIVE_VAULT)) === vaultId) await del('kv', KV_ACTIVE_VAULT)
}

export async function removeVault(vaultId: string): Promise<void> {
  if (inFlight(vaultId)) throw other('A sync is running on this vault. Wait for it to finish.')
  await forgetVault(vaultId)
}

export async function deleteRemoteVault(vaultId: string): Promise<void> {
  if (inFlight(vaultId)) throw other('A sync is running on this vault. Wait for it to finish.')
  await bearerCall((bearer) => api.deleteVault(bearer, vaultId))
  await forgetVault(vaultId)
}

export async function forgetVaultsForServer(server: string): Promise<void> {
  for (const vault of await all<StoredVault>('vaults')) {
    if (vault.server_url === server) await forgetVault(vault.id)
  }
}

// The pending diff for one vault, without transferring anything.
async function vaultState(vault: StoredVault): Promise<VaultStateInfo['state']> {
  if (vault.server_url !== serverUrl) return { kind: 'foreign' }
  if (inFlight(vault.id)) return { kind: 'syncing' }
  const pending = pendingConflicts(vault.id)
  if (pending > 0) return { kind: 'conflicts', count: pending, awaiting_resolution: true }
  const keys = keysFor(vault.id)
  if (!keys) return { kind: 'no_key' }
  let handle: FileSystemDirectoryHandle
  try {
    handle = await handleFor(vault)
  } catch (error) {
    return { kind: 'error', error_kind: 'other', message: (error as { message: string }).message }
  }
  if (
    typeof handle.queryPermission === 'function' &&
    (await handle.queryPermission({ mode: 'readwrite' })) !== 'granted'
  ) {
    return { kind: 'needs_access' }
  }
  try {
    const local = await loadLocalState(vault, handle, keys)
    const remote = await fetchRemoteManifest(vault, keys, local.state)
    await saveSyncState(vault.id, local.state)
    const diff = await diffManifests(local.state.base, local.working, remote, local.ignore)
    if (diff.conflicts.length > 0) {
      return { kind: 'conflicts', count: diff.conflicts.length, awaiting_resolution: false }
    }
    if (diff.upload.length > 0 || diff.download.length > 0) {
      return { kind: 'pending', uploads: diff.upload.length, downloads: diff.download.length }
    }
    return { kind: 'up_to_date' }
  } catch (error) {
    const failure = error as { kind?: string; message?: string }
    if ((error as DOMException)?.name === 'NotAllowedError') return { kind: 'needs_access' }
    return {
      kind: 'error',
      error_kind: isErrorKind(failure.kind) ? failure.kind : 'other',
      message: failure.message ?? String(error),
    }
  }
}

export async function getVaultStates(): Promise<VaultStateInfo[]> {
  const states: VaultStateInfo[] = []
  for (const vault of await listVaults()) {
    states.push({
      id: vault.id,
      name: vault.name,
      local_path: vault.folder_name,
      server_url: vault.server_url,
      state: await vaultState(vault),
      last_synced: await lastSynced(vault.id),
    })
  }
  return states
}

// The passphrase again after a reload: derive, validate, remember.
export async function unlockVault(vaultId: string, passphrase: string): Promise<void> {
  const vault = await vaultById(vaultId)
  const keys = await deriveKeys(vault.id, passphrase)
  try {
    await validatePassphrase(vault, keys)
  } catch (error) {
    keys.free()
    throw error
  }
  rememberKeys(vault.id, keys)
  await startDriver(vault.id)
}
