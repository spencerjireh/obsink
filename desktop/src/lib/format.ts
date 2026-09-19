import type { SyncPhase, SyncResult, UsageInfo } from '../types'

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
