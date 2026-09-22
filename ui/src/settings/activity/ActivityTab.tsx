import { useState } from 'react'
import type { VaultStateInfo } from '../../types'
import { useActivity } from '../../hooks/useActivity'
import { activityLine } from '../../lib/activity'
import { formatRelative } from '../../lib/format'
import { EmptyState } from '../../components/EmptyState'

export function ActivityTab({
  states,
  onError,
}: {
  states: VaultStateInfo[]
  onError: (error: unknown) => void
}) {
  const [vaultId, setVaultId] = useState<string>('')
  const { events } = useActivity(vaultId || null, 200, onError)
  const names = new Map(states.map((s) => [s.id, s.name]))

  return (
    <section className="section" aria-labelledby="activity-heading">
      <div className="section__heading">
        <h2 id="activity-heading">Activity</h2>
        <label className="inline-field">
          <span>Vault</span>
          <select value={vaultId} onChange={(event) => setVaultId(event.target.value)}>
            <option value="">All vaults</option>
            {states.map((s) => (
              <option key={s.id} value={s.id}>
                {s.name}
              </option>
            ))}
          </select>
        </label>
      </div>
      {events.length === 0 ? (
        <EmptyState>Run a sync to see uploads and downloads.</EmptyState>
      ) : (
        <ol className="activity-list">
          {events.map((event, index) => (
            <li
              key={`${event.at}-${index}`}
              className={`activity-row activity-row--${event.kind}`}
              data-testid="activityRow"
              data-kind={event.kind}
            >
              <span className="activity-row__time">{formatRelative(event.at)}</span>
              {!vaultId ? (
                <span className="activity-row__vault">{names.get(event.vault_id) ?? '?'}</span>
              ) : null}
              <span className="activity-row__text mono">{activityLine(event)}</span>
            </li>
          ))}
        </ol>
      )}
    </section>
  )
}
