import type { ConflictPreview, ProgressEvent, Resolution, SyncResponse } from '@obsink/ui'
import type { VaultSyncState } from '../shared/db'
import { recordError, recordSync } from './activity'
import { emit, stateChanged } from './bus'
import { other } from './errors'
import { readFile } from './fs'
import { keysFor } from './keys'
import { fetchRemoteManifest, loadSyncState, remoteChanged, saveSyncState } from './manifest'
import { api, bearerCall } from './session'
import {
  completeSync,
  detectChanges,
  localExists,
  prepareSync,
  type Cycle,
  type SyncPlan,
} from './sync'
import { handleFor, vaultById } from './vaults'
import { wasm } from './wasm'

// The browser's driver above the engine, one per unlocked vault, paced like
// `core::daemon`: a quick poll while there was activity, a slow one when
// idle, exponential backoff after a fatal error, and never a resolution of
// its own (conflicts are deferred and reported until the user answers).
// There is no file watcher in a browser, so a poll also rescans the folder
// through the hash cache. Everything stops when the tab closes.

const POLL_ACTIVE_MS = 5_000
const POLL_IDLE_MS = 60_000
const ACTIVE_WINDOW_MS = 60_000
const BACKOFF_BASE_MS = 5_000
const BACKOFF_MAX_MS = 5 * 60_000

type Pending = { plan: SyncPlan; state: VaultSyncState }

type Driver = {
  vaultId: string
  running: boolean
  // The plan a cycle stopped on; `resolveConflict` completes it.
  pending: Pending | null
  lastActivity: number
  failures: number
  suppressedUntil: number
  timer: ReturnType<typeof setTimeout> | null
  // Cycles on one vault never overlap: a manual sync queues behind the poll.
  queue: Promise<unknown>
}

const drivers = new Map<string, Driver>()

// The page reports a hidden tab or an offline browser; polling pauses (no
// wasted cycles, no error rows while offline) and resumes with a poll.
let paused = false

export function setPaused(value: boolean): void {
  if (paused === value) return
  paused = value
  if (!paused) for (const driver of drivers.values()) schedule(driver, 0)
}

function driverFor(vaultId: string): Driver {
  let driver = drivers.get(vaultId)
  if (!driver) {
    driver = {
      vaultId,
      running: false,
      pending: null,
      lastActivity: 0,
      failures: 0,
      suppressedUntil: 0,
      timer: null,
      queue: Promise.resolve(),
    }
    drivers.set(vaultId, driver)
  }
  return driver
}

export function inFlight(vaultId: string): boolean {
  return drivers.get(vaultId)?.running ?? false
}

export function pendingConflicts(vaultId: string): number {
  return drivers.get(vaultId)?.pending?.plan.conflicts.length ?? 0
}

function errorText(error: unknown): string {
  return (error as { message?: string })?.message ?? String(error)
}

async function cycleFor(vaultId: string, quiet = false): Promise<Cycle> {
  const vault = await vaultById(vaultId)
  const keys = keysFor(vaultId)
  if (!keys) throw other('No key for this vault in this browser. Enter the passphrase first.')
  const root = await handleFor(vault)
  const progress = quiet
    ? () => undefined
    : (event: ProgressEvent) => emit('sync://progress', { vault_id: vaultId, event })
  return { vault, root, keys, progress }
}

// Run one thing on a vault at a time.
function serialized<T>(driver: Driver, work: () => Promise<T>): Promise<T> {
  const run = driver.queue.then(work, work)
  driver.queue = run.catch(() => undefined)
  return run
}

async function guarded<T>(driver: Driver, work: () => Promise<T>): Promise<T> {
  driver.running = true
  stateChanged(driver.vaultId)
  try {
    return await work()
  } finally {
    driver.running = false
    stateChanged(driver.vaultId)
  }
}

async function backoffWait(failures: number): Promise<number> {
  const core = await wasm()
  return core.backoffWaitMs(BACKOFF_BASE_MS, BACKOFF_MAX_MS, failures)
}

async function noteFailure(driver: Driver, message: string): Promise<void> {
  driver.suppressedUntil = Date.now() + (await backoffWait(driver.failures))
  driver.failures += 1
  await recordError(driver.vaultId, message)
}

// After `completeSync`: late 409s become a conflict-only plan the UI can
// resolve in another round; otherwise the vault has no pending plan.
async function finishCycle(
  driver: Driver,
  result: Awaited<ReturnType<typeof completeSync>>,
  state: VaultSyncState,
): Promise<SyncResponse> {
  driver.pending =
    result.conflicts.length > 0
      ? { plan: { upload: [], download: [], conflicts: result.conflicts, failures: [] }, state }
      : null
  await recordSync(driver.vaultId, result)
  if (result.upload.length > 0 || result.download.length > 0) driver.lastActivity = Date.now()
  const fatal = result.failures.find((failure) => failure.fatal)
  if (fatal || result.checkpoint_error) {
    await noteFailure(driver, result.checkpoint_error ?? fatal?.error ?? 'sync failed')
  } else {
    driver.failures = 0
  }
  return { completed_result: result, pending_conflicts: result.conflicts }
}

// A manual "Sync now": prepare, and either complete (no conflicts) or hold
// the plan for the resolver.
async function runSync(driver: Driver): Promise<SyncResponse> {
  const cycle = await cycleFor(driver.vaultId)
  // A fresh cycle supersedes any plan left over from an earlier one.
  driver.pending = null
  const { plan, state } = await prepareSync(cycle)
  if (plan.conflicts.length === 0) {
    const result = await completeSync(cycle, plan, [], state)
    return finishCycle(driver, result, state)
  }
  driver.pending = { plan, state }
  return { completed_result: null, pending_conflicts: plan.conflicts }
}

export function syncVault(vaultId: string): Promise<SyncResponse> {
  const driver = driverFor(vaultId)
  return serialized(driver, () =>
    guarded(driver, async () => {
      try {
        return await runSync(driver)
      } catch (error) {
        await noteFailure(driver, errorText(error))
        throw error
      }
    }),
  )
}

// Every conflict of the pending plan without an explicit choice is deferred.
function deferTheRest(plan: SyncPlan, resolutions: Resolution[]): Resolution[] {
  const answered = new Set(resolutions.map((resolution) => resolution.path))
  const missing = plan.conflicts
    .filter((conflict) => !answered.has(conflict.path))
    .map((conflict) => ({ path: conflict.path, choice: 'Defer' }) as unknown as Resolution)
  return [...resolutions, ...missing]
}

export function resolveConflict(vaultId: string, resolutions: Resolution[]): Promise<SyncResponse> {
  const driver = driverFor(vaultId)
  return serialized(driver, () =>
    guarded(driver, async () => {
      const pending = driver.pending
      if (!pending) throw other('No sync is waiting for a decision on this vault.')
      const cycle = await cycleFor(vaultId)
      try {
        const all = deferTheRest(pending.plan, resolutions)
        const result = await completeSync(cycle, pending.plan, all, pending.state)
        return await finishCycle(driver, result, pending.state)
      } catch (error) {
        await noteFailure(driver, errorText(error))
        throw error
      }
    }),
  )
}

// The polling loop: a cycle when the server or the folder changed, with
// every conflict deferred so the rest keeps syncing.
async function poll(driver: Driver): Promise<void> {
  if (paused || driver.running || Date.now() < driver.suppressedUntil || driver.pending) return
  if (!keysFor(driver.vaultId)) return
  await serialized(driver, async () => {
    try {
      const cycle = await cycleFor(driver.vaultId, true)
      const state = await loadSyncState(cycle.vault.id)
      let changed = await remoteChanged(cycle.vault, cycle.keys, state)
      if (changed) {
        await saveSyncState(cycle.vault.id, state)
      } else {
        // No watcher here: the folder is rescanned (stat-only through the
        // hash cache) to find local edits. Nothing is transferred yet.
        const pending = await detectChanges(cycle)
        changed = pending.upload > 0 || pending.download > 0 || pending.conflicts > 0
      }
      if (!changed) return
    } catch (error) {
      await noteFailure(driver, errorText(error))
      stateChanged(driver.vaultId)
      return
    }
    driver.lastActivity = Date.now()
    await guarded(driver, async () => {
      try {
        const cycle = await cycleFor(driver.vaultId)
        const { plan, state } = await prepareSync(cycle)
        const result = await completeSync(cycle, plan, deferTheRest(plan, []), state)
        await finishCycle(driver, result, state)
      } catch (error) {
        await noteFailure(driver, errorText(error))
      }
    })
  })
}

function schedule(driver: Driver, delay?: number): void {
  if (driver.timer) clearTimeout(driver.timer)
  const since = Date.now() - driver.lastActivity
  const interval = delay ?? (since < ACTIVE_WINDOW_MS ? POLL_ACTIVE_MS : POLL_IDLE_MS)
  driver.timer = setTimeout(async () => {
    driver.timer = null
    if (!drivers.has(driver.vaultId)) return
    await poll(driver)
    if (drivers.has(driver.vaultId)) schedule(driver)
  }, interval)
}

// Start polling a vault whose key is in memory (after add or unlock).
export function startDriver(vaultId: string): void {
  const driver = driverFor(vaultId)
  driver.lastActivity = Date.now()
  schedule(driver)
}

export function stopDriver(vaultId: string): void {
  const driver = drivers.get(vaultId)
  if (!driver) return
  if (driver.timer) clearTimeout(driver.timer)
  drivers.delete(vaultId)
}

// Both sides of a conflict as text, for the side-by-side preview.
export async function getConflictPreview(vaultId: string, path: string): Promise<ConflictPreview> {
  const cycle = await cycleFor(vaultId, true)
  const decoder = new TextDecoder()
  const local_deleted = !(await localExists(cycle.root, path))
  const local_text = local_deleted ? '' : decoder.decode(await readFile(cycle.root, path))
  const state = await loadSyncState(vaultId)
  const remote =
    state.remote_cache?.manifest ?? (await fetchRemoteManifest(cycle.vault, cycle.keys, state))
  const entry = remote[path]
  const remote_deleted = !entry || entry.deleted
  let remote_text = ''
  if (!remote_deleted) {
    const blob = await bearerCall((bearer) =>
      api.getFile(bearer, vaultId, cycle.keys.pathToken(path)),
    )
    remote_text = decoder.decode(cycle.keys.decrypt(blob))
  }
  return { path, local_text, remote_text, local_deleted, remote_deleted }
}
