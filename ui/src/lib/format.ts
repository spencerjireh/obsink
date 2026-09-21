import type { InviteStatus, SyncPhase, SyncResult, UsageInfo, VaultUsage } from '../types'

export function formatBytes(value: number): string {
  if (value < 1024) return `${value} B`
  const units = ['KiB', 'MiB', 'GiB', 'TiB']
  let size = value / 1024
  let unit = 0
  while (size >= 1024 && unit < units.length - 1) {
    size /= 1024
    unit += 1
  }
  return `${size.toFixed(size < 10 ? 1 : 0)} ${units[unit]}`
}

export function usageLine(usage: UsageInfo | null): string {
  if (!usage) return ''
  const vaults =
    usage.max_vaults === null
      ? `${usage.vaults.length} vaults`
      : `${usage.vaults.length}/${usage.max_vaults} vaults`
  const cap =
    usage.max_vault_bytes === null ? '' : ` · ${formatBytes(usage.max_vault_bytes)} per vault`
  return ` · ${formatBytes(usage.total_bytes)} used · ${vaults}${cap}`
}

export function vaultUsageLine(usage: VaultUsage | null): string {
  if (!usage) return ''
  return usage.max === null
    ? formatBytes(usage.bytes)
    : `${formatBytes(usage.bytes)} of ${formatBytes(usage.max)}`
}

export function formatUnix(value: number): string {
  if (!value) {
    return 'Unknown'
  }

  return new Date(value * 1000).toLocaleString()
}

export function countRemoteChanges(diff: SyncResult): number {
  return diff.download.length + diff.conflicts.length
}

export function phaseLabel(phase: SyncPhase): string {
  switch (phase) {
    case 'Downloading':
      return 'Downloading'
    case 'ResolvingConflicts':
      return 'Resolving conflicts'
    case 'Uploading':
      return 'Uploading'
  }
}

export function plural(count: number, noun: string): string {
  return `${count} ${noun}${count === 1 ? '' : 's'}`
}

export function inviteStatusLabel(status: InviteStatus): string {
  switch (status) {
    case 'active':
      return 'Active'
    case 'used':
      return 'Used'
    case 'expired':
      return 'Expired'
  }
}

// "synced 2 min ago" style timestamps (unix seconds); null means never.
export function formatRelative(value: number | null, now = Date.now() / 1000): string {
  if (!value) return 'Never synced'
  const seconds = Math.max(0, Math.floor(now - value))
  if (seconds < 60) return 'Just now'
  const minutes = Math.floor(seconds / 60)
  if (minutes < 60) return minutes === 1 ? '1 minute ago' : `${minutes} minutes ago`
  const hours = Math.floor(minutes / 60)
  if (hours < 24) return hours === 1 ? '1 hour ago' : `${hours} hours ago`
  const days = Math.floor(hours / 24)
  if (days === 1) return 'Yesterday'
  if (days < 7) return `${days} days ago`
  return new Date(value * 1000).toLocaleDateString()
}
