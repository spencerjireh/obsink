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
    case 'foreign':
      return 'On another server'
    case 'no_key':
      return 'Needs passphrase'
  }
}

export function stateTone(info: VaultStateInfo): StateTone {
  switch (info.state.kind) {
    case 'up_to_date':
      return 'ok'
    case 'pending':
    case 'syncing':
    case 'no_key':
      return 'pending'
    case 'conflicts':
      return 'conflict'
    case 'error':
      return 'error'
    case 'foreign':
      return 'muted'
  }
}

// Files the other side changed: what the stale notice counts.
export function remoteChanges(info: VaultStateInfo | null): number {
  if (!info) return 0
  if (info.state.kind === 'pending') return info.state.downloads
  if (info.state.kind === 'conflicts') return info.state.count
  return 0
}

export function canSync(info: VaultStateInfo | null): boolean {
  return !!info && info.state.kind !== 'foreign' && info.state.kind !== 'no_key'
}

// One line for every vault at once, worst state first.
export function globalLine(states: VaultStateInfo[]): string {
  if (states.length === 0) return 'No vaults yet.'
  if (states.some((s) => s.state.kind === 'error' && s.state.error_kind === 'network')) {
    return 'Offline'
  }
  const conflicts = states.reduce(
    (sum, s) => sum + (s.state.kind === 'conflicts' ? s.state.count : 0),
    0,
  )
  if (conflicts > 0) return plural(conflicts, 'conflict')
  if (states.some((s) => s.state.kind === 'syncing')) return 'Syncing…'
  if (states.some((s) => s.state.kind === 'error')) return 'Error'
  const latest = states.reduce<number | null>(
    (max, s) => (s.last_synced && (!max || s.last_synced > max) ? s.last_synced : max),
    null,
  )
  const pending = states.some((s) => s.state.kind === 'pending')
  const head = pending ? 'Changes pending' : 'Up to date'
  return latest ? `${head} · synced ${formatRelative(latest).toLowerCase()}` : head
}
