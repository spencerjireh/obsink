import type { SyncStatus } from '../types'

export function StatusCounts({ status }: { status: SyncStatus | null }) {
  const counts = [
    { label: 'Uploads', value: status?.pending_uploads ?? 0 },
    { label: 'Downloads', value: status?.pending_downloads ?? 0 },
    { label: 'Conflicts', value: status?.pending_conflicts ?? 0 },
  ]
  return (
    <div className="status-counts">
      {counts.map((count) => (
        <div key={count.label} className="status-count">
          <span className="status-count__label">{count.label}</span>
          <strong className="status-count__value">{count.value}</strong>
        </div>
      ))}
    </div>
  )
}
