import type { ProgressEvent, Resolution, SyncFailure, SyncResult } from '@obsink/ui'
import type { FileEntry, Manifest, StoredVault, VaultSyncState } from '../shared/db'
import { api, bearerCall } from './session'
import { exists, readFile, removeFile, statOf, writeFile } from './fs'
import {
  diffManifests,
  fetchRemoteManifest,
  loadLocalState,
  saveSyncState,
  type Conflict,
  type SyncAction,
} from './manifest'
import type { BatchOperation, WireFileEntry } from './api'
import { wasm, type VaultKeys } from './wasm'

// The sync cycle, as `core::sync_engine` runs it natively: `prepareSync`
// (walk, fetch, diff, apply downloads) and `completeSync` (resolutions,
// batched uploads, checkpoint). Every rule the native engine decides with
// code — batching, effective choices, copy names, the checkpoint — is
// asked of core-wasm, so the two cannot drift.

const DOWNLOAD_CONCURRENCY = 8

export type SyncPlan = {
  upload: SyncAction[]
  download: SyncAction[]
  conflicts: Conflict[]
  failures: SyncFailure[]
}

export type ProgressSink = (event: ProgressEvent) => void

// The UI's `SyncResult` with the full manifest entries on its conflicts, so
// a late 409 can be completed in another round.
export type SyncOutcome = Omit<SyncResult, 'conflicts'> & { conflicts: Conflict[] }

export type Cycle = {
  vault: StoredVault
  root: FileSystemDirectoryHandle
  keys: VaultKeys
  progress: ProgressSink
}

function isFatalStatus(status: number | undefined): boolean {
  return status === 401 || status === 403 || (status !== undefined && status >= 500)
}

// An error after which the rest of the cycle cannot succeed.
function isFatalError(error: unknown): boolean {
  const failure = error as { kind?: string; status?: number }
  if (failure?.kind === 'unauthorized' || failure?.kind === 'network') return true
  return isFatalStatus(failure?.status)
}

function message(error: unknown): string {
  const failure = error as { message?: string }
  return failure?.message ?? String(error)
}

// Deletes first, one at a time, before downloads (a case-insensitive
// filesystem must see the old name gone before the new one lands).
function orderedForApply(downloads: SyncAction[]): SyncAction[] {
  return [
    ...downloads.filter((action) => action.kind === 'DeleteLocal'),
    ...downloads.filter((action) => action.kind !== 'DeleteLocal'),
  ]
}

async function downloadTo(cycle: Cycle, path: string): Promise<number> {
  const blob = await bearerCall((bearer) =>
    api.getFile(bearer, cycle.vault.id, cycle.keys.pathToken(path)),
  )
  const plaintext = cycle.keys.decrypt(blob)
  await writeFile(cycle.root, path, plaintext)
  return plaintext.length
}

async function applyDownloads(
  cycle: Cycle,
  downloads: SyncAction[],
  failures: SyncFailure[],
): Promise<void> {
  if (downloads.length === 0) return
  cycle.progress({ Phase: 'Downloading' })
  const ordered = orderedForApply(downloads)
  const total = ordered.length
  let stop = false
  let index = 0

  const runOne = async (action: SyncAction, position: number) => {
    if (stop) return
    cycle.progress({
      FileStarted: { path: action.path, kind: action.kind, index: position, total },
    })
    try {
      if (action.kind === 'DeleteLocal') {
        await removeFile(cycle.root, action.path)
        cycle.progress({ FileCompleted: { path: action.path, bytes: 0 } })
      } else {
        const bytes = await downloadTo(cycle, action.path)
        cycle.progress({ FileCompleted: { path: action.path, bytes } })
      }
    } catch (error) {
      const fatal = isFatalError(error)
      failures.push({ path: action.path, kind: action.kind, error: message(error), fatal })
      cycle.progress({ FileFailed: { path: action.path, error: message(error) } })
      if (fatal) stop = true
    }
  }

  // Deletes sequentially, then downloads through a small pool.
  for (const action of ordered) {
    if (action.kind !== 'DeleteLocal') break
    await runOne(action, index++)
  }
  const remaining = ordered.slice(index)
  const workers = Array.from(
    { length: Math.min(DOWNLOAD_CONCURRENCY, remaining.length) },
    async () => {
      while (!stop) {
        const next = remaining.shift()
        if (!next) return
        await runOne(next, index++)
      }
    },
  )
  await Promise.all(workers)
}

// The pending diff without transferring anything (the driver's change
// detection, desktop's `vault_diff`).
export async function detectChanges(
  cycle: Cycle,
): Promise<{ upload: number; download: number; conflicts: number }> {
  const local = await loadLocalState(cycle.vault, cycle.root, cycle.keys)
  const remote = await fetchRemoteManifest(cycle.vault, cycle.keys, local.state)
  await saveSyncState(cycle.vault.id, local.state)
  const diff = await diffManifests(local.state.base, local.working, remote, local.ignore)
  return {
    upload: diff.upload.length,
    download: diff.download.length,
    conflicts: diff.conflicts.length,
  }
}

// Walk, fetch, diff, apply what comes down. Uploads wait for `completeSync`
// so conflicts can be answered in between.
export async function prepareSync(
  cycle: Cycle,
): Promise<{ plan: SyncPlan; state: VaultSyncState }> {
  const local = await loadLocalState(cycle.vault, cycle.root, cycle.keys)
  const remote = await fetchRemoteManifest(cycle.vault, cycle.keys, local.state)
  const diff = await diffManifests(local.state.base, local.working, remote, local.ignore)
  const failures: SyncFailure[] = []
  await applyDownloads(cycle, diff.download, failures)
  await saveSyncState(cycle.vault.id, local.state)
  return {
    plan: { upload: diff.upload, download: diff.download, conflicts: diff.conflicts, failures },
    state: local.state,
  }
}

async function uploadActionFor(cycle: Cycle, path: string): Promise<SyncAction> {
  const bytes = await readFile(cycle.root, path)
  const stat = statOf(await fileStat(cycle.root, path))
  return {
    path,
    kind: 'Upload',
    local: {
      hash: cycle.keys.contentHmac(bytes),
      modified: stat.mtime_secs,
      size: stat.size,
      deleted: false,
      encPath: '',
    },
    remote: null,
  }
}

async function fileStat(root: FileSystemDirectoryHandle, path: string) {
  const parts = path.split('/')
  const name = parts.pop() as string
  let dir = root
  for (const part of parts) dir = await dir.getDirectoryHandle(part)
  const file = await (await dir.getFileHandle(name)).getFile()
  return { size: file.size, mtimeMs: file.lastModified }
}

// Every conflict needs an answer; `KeepBoth` keeps the remote version
// under a `.conflict` name and uploads both.
async function applyResolutions(
  cycle: Cycle,
  plan: SyncPlan,
  resolutions: Resolution[],
): Promise<{ uploads: SyncAction[]; deferred: Conflict[]; failures: SyncFailure[] }> {
  const core = await wasm()
  const uploads: SyncAction[] = [...plan.upload]
  const deferred: Conflict[] = []
  const failures: SyncFailure[] = []
  if (plan.conflicts.length > 0) cycle.progress({ Phase: 'ResolvingConflicts' })
  for (const conflict of plan.conflicts) {
    const answer = resolutions.find((resolution) => resolution.path === conflict.path)
    if (!answer)
      throw { kind: 'other', message: `missing resolution for conflict at ${conflict.path}` }
    const choice = JSON.parse(
      core.effectiveChoice(JSON.stringify(answer.choice), JSON.stringify(conflict)),
    ) as Resolution['choice'] | 'Defer'
    try {
      if (choice === 'Defer') {
        deferred.push(conflict)
      } else if (choice === 'KeepLocal') {
        uploads.push(JSON.parse(core.conflictToUpload(JSON.stringify(conflict))) as SyncAction)
      } else if (choice === 'KeepRemote') {
        if (conflict.remote.deleted) await removeFile(cycle.root, conflict.path)
        else await downloadTo(cycle, conflict.path)
      } else {
        const copy = core.conflictCopyPath(conflict.path)
        const blob = await bearerCall((bearer) =>
          api.getFile(bearer, cycle.vault.id, cycle.keys.pathToken(conflict.path)),
        )
        await writeFile(cycle.root, copy, cycle.keys.decrypt(blob))
        uploads.push(JSON.parse(core.conflictToUpload(JSON.stringify(conflict))) as SyncAction)
        uploads.push(await uploadActionFor(cycle, copy))
      }
    } catch (error) {
      failures.push({
        path: conflict.path,
        kind: 'Download',
        error: message(error),
        fatal: isFatalError(error),
      })
    }
  }
  return { uploads, deferred, failures }
}

function wireEntry(entry: WireFileEntry | null, fallback: FileEntry | null): FileEntry {
  if (entry) {
    return {
      hash: entry.hash,
      modified: entry.modified,
      size: entry.size,
      deleted: entry.deleted ?? false,
      encPath: entry.encPath ?? '',
    }
  }
  return { hash: fallback?.hash ?? '', modified: 0, size: 0, deleted: true, encPath: '' }
}

async function runUploads(
  cycle: Cycle,
  uploads: SyncAction[],
): Promise<{ done: SyncAction[]; late: Conflict[]; failures: SyncFailure[] }> {
  const core = await wasm()
  const done: SyncAction[] = []
  const late: Conflict[] = []
  const failures: SyncFailure[] = []
  if (uploads.length === 0) return { done, late, failures }
  cycle.progress({ Phase: 'Uploading' })
  const sizes = uploads.map((action) => (action.kind === 'Upload' ? (action.local?.size ?? 0) : 0))
  const ranges = JSON.parse(core.chunkUploads(JSON.stringify(sizes))) as [number, number][]
  const total = uploads.length
  let stop = false

  for (const [start, end] of ranges) {
    if (stop) break
    const batch = uploads.slice(start, end)
    const operations: BatchOperation[] = []
    const contents = new Map<number, Uint8Array>()
    const members: SyncAction[] = []
    for (const [offset, action] of batch.entries()) {
      cycle.progress({
        FileStarted: { path: action.path, kind: action.kind, index: start + offset, total },
      })
      const parentHash = action.remote?.hash || undefined
      try {
        if (action.kind === 'Upload') {
          const plaintext = await readFile(cycle.root, action.path)
          operations.push({
            action: 'put',
            path: cycle.keys.pathToken(action.path),
            parentHash,
            contentHash: action.local?.hash ?? cycle.keys.contentHmac(plaintext),
            encPath: cycle.keys.encryptPath(action.path),
          })
          contents.set(operations.length - 1, cycle.keys.encrypt(plaintext))
          members.push(action)
        } else if (action.kind === 'DeleteRemote') {
          operations.push({ action: 'delete', path: cycle.keys.pathToken(action.path), parentHash })
          members.push(action)
        } else {
          done.push(action)
        }
      } catch (error) {
        failures.push({ path: action.path, kind: action.kind, error: message(error), fatal: false })
      }
    }
    if (operations.length === 0) continue

    let results
    try {
      results = await bearerCall((bearer) =>
        api.batch(bearer, cycle.vault.id, operations, contents),
      )
    } catch (error) {
      const fatal = isFatalError(error)
      for (const action of members) {
        failures.push({ path: action.path, kind: action.kind, error: message(error), fatal })
        cycle.progress({ FileFailed: { path: action.path, error: message(error) } })
      }
      if (fatal) stop = true
      continue
    }
    for (const [offset, result] of results.entries()) {
      const action = members[offset]
      if (result.status >= 200 && result.status < 300) {
        done.push(action)
        cycle.progress({ FileCompleted: { path: action.path, bytes: action.local?.size ?? 0 } })
      } else if (result.status === 409) {
        late.push({
          path: action.path,
          local: action.local ?? wireEntry(null, null),
          remote: wireEntry(result.conflict?.current ?? null, action.remote),
        })
      } else {
        const fatal = isFatalStatus(result.status)
        failures.push({
          path: action.path,
          kind: action.kind,
          error: `server returned ${result.status}`,
          fatal,
        })
        cycle.progress({
          FileFailed: { path: action.path, error: `server returned ${result.status}` },
        })
        if (fatal) stop = true
      }
    }
  }
  return { done, late, failures }
}

// Resolutions, uploads, then the checkpoint: the re-fetched server manifest
// with held-back paths (failures, deferred conflicts) kept at their old base.
export async function completeSync(
  cycle: Cycle,
  plan: SyncPlan,
  resolutions: Resolution[],
  state: VaultSyncState,
): Promise<SyncOutcome> {
  const core = await wasm()
  const resolved = await applyResolutions(cycle, plan, resolutions)
  const uploaded = await runUploads(cycle, resolved.uploads)
  const failures = [...plan.failures, ...resolved.failures, ...uploaded.failures]
  const holdBack = new Set<string>([
    ...failures.map((failure) => failure.path),
    ...resolved.deferred.map((conflict) => conflict.path),
    ...uploaded.late.map((conflict) => conflict.path),
  ])
  cycle.progress({
    Done: {
      uploaded: uploaded.done.filter((action) => action.kind === 'Upload').length,
      downloaded: plan.download.filter((action) => action.kind === 'Download').length,
      failed: failures.length,
    },
  })

  let checkpoint_error: string | undefined
  if (!failures.some((failure) => failure.fatal)) {
    try {
      const refetched = await fetchRemoteManifest(cycle.vault, cycle.keys, state)
      state.base = JSON.parse(
        core.checkpointManifest(
          JSON.stringify(state.base),
          JSON.stringify(refetched),
          JSON.stringify([...holdBack]),
        ),
      ) as Manifest
      await saveSyncState(cycle.vault.id, state)
      await reportCheckpoint(cycle.vault.id, state)
    } catch (error) {
      checkpoint_error = message(error)
    }
  }

  return {
    upload: uploaded.done,
    download: plan.download,
    conflicts: [...resolved.deferred, ...uploaded.late],
    failures,
    ...(checkpoint_error ? { checkpoint_error } : {}),
  }
}

// The manifest revision is the ETag (`"<n>"`), as core reads it.
export function revisionOf(etag: string | null | undefined): number | null {
  if (!etag) return null
  const parsed = Number.parseInt(etag.trim().replace(/^"|"$/g, ''), 10)
  return Number.isFinite(parsed) ? parsed : null
}

// Spec §4.3: tell the server which revision this device now holds. Best
// effort: a failure is logged and never fails the checkpoint.
async function reportCheckpoint(vaultId: string, state: VaultSyncState): Promise<void> {
  const revision = revisionOf(state.remote_cache?.etag)
  if (revision === null) return
  try {
    await bearerCall((bearer) => api.attachDevice(bearer, vaultId, revision))
  } catch (error) {
    console.warn(`checkpoint report for ${vaultId} failed: ${message(error)}`)
  }
}

// Whether the folder has a file at `path` (for the conflict preview).
export function localExists(root: FileSystemDirectoryHandle, path: string): Promise<boolean> {
  return exists(root, path)
}
