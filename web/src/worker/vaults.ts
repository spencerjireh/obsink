import type {
  CommandErrorKind,
  CreateVaultRequest,
  DevicePlatform,
  DownloadVaultRequest,
  LocalVault,
  VaultDevice,
  VaultStateInfo,
} from '@obsink/ui'
import type { VaultSummary } from './api'
import {
  all,
  del,
  get,
  KV_ACTIVE_VAULT,
  listVaults as storedVaults,
  put,
  type StoredVault,
} from '../shared/db'
import { other } from './errors'
import {
  forgetKeys,
  keysFor,
  keysFromRaw,
  newWrappedVaultKey,
  rememberKeys,
  unwrapVaultKeys,
} from './keys'
import { diffManifests, fetchRemoteManifest, loadLocalState, saveSyncState } from './manifest'
import { api, bearerCall, serverUrl } from './session'
import { forgetActivity, lastSynced } from './activity'
import { inFlight, pendingConflicts, startDriver, stopDriver } from './driver'

const MAX_FILE_SIZE = 50 * 1024 * 1024

function isErrorKind(kind: unknown): kind is CommandErrorKind {
  return kind === 'unauthorized' || kind === 'network' || kind === 'server' || kind === 'other'
}

export async function vaultById(vaultId: string): Promise<StoredVault> {
  const vault = await get<StoredVault>('vaults', vaultId)
  if (!vault) throw other('This vault is not in this browser. Download it first.')
  return vault
}

export async function handleFor(vault: StoredVault): Promise<FileSystemDirectoryHandle> {
  const handle = await get<FileSystemDirectoryHandle>('handles', vault.handle_id)
  if (!handle) throw other('Folder not found. Remove the vault and download it again.')
  return handle
}

function localVault(vault: StoredVault): LocalVault {
  return { id: vault.id, name: vault.name, local_path: vault.folder_name }
}

function platformOf(value: string): DevicePlatform {
  return value === 'macos' || value === 'ios' || value === 'browser' || value === 'cli'
    ? value
    : 'unknown'
}

function devicesOf(summary: VaultSummary): VaultDevice[] {
  return (summary.devices ?? []).map((device) => ({
    id: device.id,
    name: device.name,
    platform: platformOf(device.platform),
    last_synced: device.last_synced ?? null,
    last_revision: device.last_revision ?? null,
  }))
}

// The vault's key from its member blob, remembered for the tab.
async function unlockVault(summary: VaultSummary): Promise<void> {
  if (!summary.wrapped_key) {
    throw other('This vault has no key for the account (it was created before the passphrase).')
  }
  const keys = await unwrapVaultKeys(summary.id, summary.wrapped_key)
  rememberKeys(summary.id, keys)
}

// After the account key arrives (unlock, set passphrase): every stored
// vault gets its key and its driver.
export async function unlockStoredVaults(): Promise<void> {
  const stored = await storedVaults()
  if (stored.length === 0) return
  const summaries = await bearerCall((bearer) => api.listVaults(bearer))
  for (const vault of stored) {
    const summary = summaries.find((entry) => entry.id === vault.id)
    if (!summary || keysFor(vault.id)) continue
    try {
      await unlockVault(summary)
      await startDriver(vault.id)
    } catch {
      // Left locked; the vault page says so.
    }
  }
}

function mintVaultId(): string {
  return `vault_${crypto.randomUUID()}`
}

// Spec §12.2: a fresh vault key wrapped under the account key, the vault on
// the server, this browser attached, the driver started.
export async function createVault(request: CreateVaultRequest): Promise<LocalVault> {
  if (!request.local_path.trim()) throw other('Choose a folder for the vault.')
  if (!request.vault_name.trim()) throw other('Enter a vault name.')
  const handle = await get<FileSystemDirectoryHandle>('handles', request.local_path)
  if (!handle) throw other('Choose a folder for the vault.')
  const id = mintVaultId()
  const { key, wrapped } = await newWrappedVaultKey(id)
  const summary = await bearerCall((bearer) =>
    api.createVault(bearer, {
      id,
      name: request.vault_name.trim(),
      wrapped_key: wrapped,
      max_file_size: MAX_FILE_SIZE,
    }),
  )
  rememberKeys(summary.id, await keysFromRaw(key))
  return adopt(summary, request.local_path, handle.name)
}

// Spec §12.3: an existing vault of the account into a folder here.
export async function downloadVault(request: DownloadVaultRequest): Promise<LocalVault> {
  if (!request.local_path.trim()) throw other('Choose a folder for the vault.')
  const handle = await get<FileSystemDirectoryHandle>('handles', request.local_path)
  if (!handle) throw other('Choose a folder for the vault.')
  const summary = (await bearerCall((bearer) => api.listVaults(bearer))).find(
    (vault) => vault.id === request.vault_id,
  )
  if (!summary) throw other(`Vault ${request.vault_id} is not one of this account's.`)
  await unlockVault(summary)
  return adopt(summary, request.local_path, handle.name)
}

async function adopt(
  summary: VaultSummary,
  handleId: string,
  folderName: string,
): Promise<LocalVault> {
  const vault: StoredVault = {
    id: summary.id,
    name: summary.name,
    server_url: serverUrl,
    handle_id: handleId,
    folder_name: folderName,
    ignore: [],
    created: Date.now(),
  }
  await put('vaults', vault.id, vault)
  await put('kv', KV_ACTIVE_VAULT, vault.id)
  await bearerCall((bearer) => api.attachDevice(bearer, vault.id, null))
  await startDriver(vault.id)
  return localVault(vault)
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
  try {
    await bearerCall((bearer) => api.detachDevice(bearer, vaultId))
  } catch {
    // Best effort: the server learns on the next sign-in or attach.
  }
  await forgetVault(vaultId)
}

export async function deleteRemoteVault(vaultId: string): Promise<void> {
  if (inFlight(vaultId)) throw other('A sync is running on this vault. Wait for it to finish.')
  await bearerCall((bearer) => api.deleteVault(bearer, vaultId))
  await forgetVault(vaultId)
}

// Spec §4.3 `PATCH /vaults/:id`; the stored copy follows so a signed-out
// page shows the new name too.
export async function renameVault(vaultId: string, name: string): Promise<void> {
  const trimmed = name.trim()
  if (!trimmed) throw other('Enter a vault name.')
  await bearerCall((bearer) => api.renameVault(bearer, vaultId, trimmed))
  const vault = await get<StoredVault>('vaults', vaultId)
  if (vault) await put('vaults', vaultId, { ...vault, name: trimmed })
}

export async function forgetVaultsForServer(server: string): Promise<void> {
  for (const vault of await all<StoredVault>('vaults')) {
    if (vault.server_url === server) await forgetVault(vault.id)
  }
}

// The pending diff for one vault this browser holds, without transferring.
async function vaultState(vault: StoredVault): Promise<VaultStateInfo['state']> {
  if (inFlight(vault.id)) return { kind: 'syncing' }
  const pending = pendingConflicts(vault.id)
  if (pending > 0) return { kind: 'conflicts', count: pending, awaiting_resolution: true }
  const keys = keysFor(vault.id)
  if (!keys) return { kind: 'locked' }
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

// Spec §15.1: every vault of the account, with its state in this browser.
// Without a session the stored vaults are listed as locked, so a signed-out
// page still shows what it holds.
export async function listVaults(): Promise<VaultStateInfo[]> {
  const stored = await storedVaults()
  let summaries: VaultSummary[] | null = null
  try {
    summaries = await bearerCall((bearer) => api.listVaults(bearer))
  } catch (error) {
    if ((error as { kind?: string })?.kind !== 'unauthorized') throw error
  }
  const states: VaultStateInfo[] = []
  const seen = new Set<string>()
  for (const summary of summaries ?? []) {
    const vault = stored.find((entry) => entry.id === summary.id)
    seen.add(summary.id)
    states.push({
      id: summary.id,
      name: summary.name,
      local_path: vault?.folder_name ?? null,
      state: vault ? await vaultState(vault) : { kind: 'not_on_device' },
      last_synced: vault ? await lastSynced(vault.id) : null,
      revision: summary.revision ?? 0,
      bytes: summary.bytes ?? 0,
      devices: devicesOf(summary),
    })
  }
  for (const vault of stored) {
    if (seen.has(vault.id)) continue
    states.push({
      id: vault.id,
      name: vault.name,
      local_path: vault.folder_name,
      state: summaries === null ? { kind: 'locked' } : { kind: 'deleted_on_server' },
      last_synced: await lastSynced(vault.id),
      revision: 0,
      bytes: 0,
      devices: [],
    })
  }
  return states
}
