import { useActivity } from '../hooks/useActivity'
import { activityLine } from '../lib/activity'
import { formatRelative } from '../lib/format'
import { EmptyState } from './EmptyState'

// Spec §15.2: this vault's log, newest first.
export function VaultActivity({
  vaultId,
  onError,
}: {
  vaultId: string
  onError: (error: unknown) => void
}) {
  const { events } = useActivity(vaultId, 50, onError)

  return (
    <section className="section" aria-labelledby="activity-heading">
      <div className="section__heading">
        <h2 id="activity-heading">Activity</h2>
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
              <span className="activity-row__text mono">{activityLine(event)}</span>
            </li>
          ))}
        </ol>
      )}
    </section>
  )
}
