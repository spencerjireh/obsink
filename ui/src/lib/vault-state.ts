import type { VaultStateInfo } from '../types'
import { formatRelative, plural } from './format'

export type StateTone = 'ok' | 'pending' | 'conflict' | 'error' | 'muted'

// The shared state vocabulary (DESIGN.md §5); iOS says the same words.
export function stateText(info: VaultStateInfo): string {
  const state = info.state
  switch (state.kind) {
    case 'up_to_date':
      return 'Up to date'
    case 'pending': {
      const parts: string[] = []
      if (state.uploads > 0) parts.push(`${state.uploads} to upload`)
      if (state.downloads > 0) parts.push(`${state.downloads} to download`)
      return parts.join(' · ') || 'Up to date'
    }
    case 'conflicts':
      return plural(state.count, 'conflict')
    case 'syncing':
      return 'Syncing…'
    case 'error':
      if (state.error_kind === 'network') return 'Offline'
      if (state.error_kind === 'unauthorized') return 'Session expired'
      return `Error: ${state.message}`
    case 'needs_access':
      return 'Needs folder access'
    case 'not_on_device':
      return 'Not on this device'
    case 'deleted_on_server':
      return 'Deleted on the server'
    case 'locked':
      return 'Locked'
  }
}

export function stateTone(info: VaultStateInfo): StateTone {
  switch (info.state.kind) {
    case 'up_to_date':
      return 'ok'
    case 'pending':
    case 'syncing':
    case 'needs_access':
    case 'locked':
      return 'pending'
    case 'conflicts':
      return 'conflict'
    case 'error':
      return 'error'
    case 'not_on_device':
    case 'deleted_on_server':
      return 'muted'
  }
}

// Whether this device holds the vault (a folder, a key): everything but
// the three account-level states.
export function onThisDevice(info: VaultStateInfo): boolean {
  return (
    info.state.kind !== 'not_on_device' &&
    info.state.kind !== 'deleted_on_server' &&
    info.state.kind !== 'locked'
  )
}

// Files the other side changed: what the stale notice counts.
export function remoteChanges(info: VaultStateInfo | null): number {
  if (!info) return 0
  if (info.state.kind === 'pending') return info.state.downloads
  if (info.state.kind === 'conflicts') return info.state.count
  return 0
}

export function canSync(info: VaultStateInfo | null): boolean {
  return !!info && onThisDevice(info) && info.state.kind !== 'needs_access'
}

// One line for every vault at once, worst state first. Only the vaults on
// this device count; the ones elsewhere get a trailing note.
export function globalLine(states: VaultStateInfo[], deviceNoun = 'this device'): string {
  if (states.length === 0) return 'No vaults yet.'
  const here = states.filter(onThisDevice)
  const elsewhere = states.length - here.length
  if (here.length === 0) return `${plural(elsewhere, 'vault')} not on ${deviceNoun}`
  if (here.some((s) => s.state.kind === 'error' && s.state.error_kind === 'network')) {
    return 'Offline'
  }
  const conflicts = here.reduce(
    (sum, s) => sum + (s.state.kind === 'conflicts' ? s.state.count : 0),
    0,
  )
  if (conflicts > 0) return plural(conflicts, 'conflict')
  if (here.some((s) => s.state.kind === 'syncing')) return 'Syncing…'
  if (here.some((s) => s.state.kind === 'error')) return 'Error'
  const latest = here.reduce<number | null>(
    (max, s) => (s.last_synced && (!max || s.last_synced > max) ? s.last_synced : max),
    null,
  )
  const pending = here.some((s) => s.state.kind === 'pending')
  const head = pending ? 'Changes pending' : 'Up to date'
  const line = latest ? `${head} · synced ${formatRelative(latest).toLowerCase()}` : head
  // The vaults elsewhere are worth a note only when everything here is fine.
  return elsewhere > 0 && !pending
    ? `${line} · ${plural(elsewhere, 'vault')} not on ${deviceNoun}`
    : line
}
