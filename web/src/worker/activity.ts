import type { ActivityEvent, SyncResult } from '@obsink/ui'
import { all, del, get, put } from '../shared/db'

// The per-vault activity log (desktop `activity.rs`): the newest 200 events
// and the time of the last completed sync, in IndexedDB.

const MAX_EVENTS = 200

type VaultActivity = { events: ActivityEvent[]; last_synced: number | null }

async function load(vaultId: string): Promise<VaultActivity> {
  return (await get<VaultActivity>('activity', vaultId)) ?? { events: [], last_synced: null }
}

function nowSeconds(): number {
  return Math.floor(Date.now() / 1000)
}

async function append(vaultId: string, events: ActivityEvent[], synced: boolean): Promise<void> {
  const log = await load(vaultId)
  log.events.push(...events)
  if (log.events.length > MAX_EVENTS) log.events.splice(0, log.events.length - MAX_EVENTS)
  if (synced) log.last_synced = nowSeconds()
  await put('activity', vaultId, log)
}

// Per-file events for what a sync did, then one `synced` summary line.
export async function recordSync(vaultId: string, result: SyncResult): Promise<void> {
  const at = nowSeconds()
  const events: ActivityEvent[] = []
  const line = (kind: ActivityEvent['kind'], path?: string, detail?: string): ActivityEvent => ({
    at,
    vault_id: vaultId,
    kind,
    ...(path ? { path } : {}),
    ...(detail ? { detail } : {}),
  })
  let uploaded = 0
  let downloaded = 0
  for (const action of result.upload) {
    if (action.kind === 'DeleteRemote') events.push(line('deleted_on_server', action.path))
    else {
      uploaded++
      events.push(line('uploaded', action.path))
    }
  }
  for (const action of result.download) {
    if (action.kind === 'DeleteLocal') events.push(line('deleted_here', action.path))
    else {
      downloaded++
      events.push(line('downloaded', action.path))
    }
  }
  for (const conflict of result.conflicts) events.push(line('conflict', conflict.path))
  for (const failure of result.failures) {
    events.push(line('error', failure.path, `${failure.fatal ? 'FATAL ' : ''}${failure.error}`))
  }
  if (result.checkpoint_error) events.push(line('error', undefined, result.checkpoint_error))
  events.push(line('synced', undefined, `↑${uploaded} ↓${downloaded}`))
  await append(vaultId, events, true)
}

export async function recordError(vaultId: string, message: string): Promise<void> {
  await append(
    vaultId,
    [{ at: nowSeconds(), vault_id: vaultId, kind: 'error', detail: message }],
    false,
  )
}

export async function lastSynced(vaultId: string): Promise<number | null> {
  return (await load(vaultId)).last_synced
}

export function forgetActivity(vaultId: string): Promise<void> {
  return del('activity', vaultId)
}

// Newest first, across every vault or one, capped at `limit`.
export async function listActivity(
  vaultId: string | null,
  limit: number,
): Promise<ActivityEvent[]> {
  const logs = vaultId ? [await load(vaultId)] : await all<VaultActivity>('activity')
  const events = logs.flatMap((log) => log.events)
  events.sort((a, b) => b.at - a.at)
  return events.slice(0, limit)
}
