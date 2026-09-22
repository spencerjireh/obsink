import type { ActivityEvent, VaultStateInfo } from '../types'
import { activityLine } from '../lib/activity'
import { formatRelative } from '../lib/format'

export function RecentList({
  events,
  states,
}: {
  events: ActivityEvent[]
  states: VaultStateInfo[]
}) {
  const names = new Map(states.map((s) => [s.id, s.name]))
  return (
    <section className="recent" aria-labelledby="recent-heading">
      <h2 id="recent-heading" className="popover__heading">
        Recent
      </h2>
      {events.length === 0 ? (
        <p className="empty-state">Run a sync to see uploads and downloads.</p>
      ) : (
        <ol className="recent__list">
          {events.map((event, index) => (
            <li
              key={`${event.at}-${index}`}
              className={`recent__item recent__item--${event.kind}`}
              data-testid="recentRow"
              data-kind={event.kind}
            >
              <span className="recent__text mono">{activityLine(event)}</span>
              <span className="recent__meta">
                {states.length > 1 ? `${names.get(event.vault_id) ?? '?'} · ` : ''}
                {formatRelative(event.at)}
              </span>
            </li>
          ))}
        </ol>
      )}
    </section>
  )
}
