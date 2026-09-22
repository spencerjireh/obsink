import type { Conflict, ConflictPreview, ResolutionChoice } from '../types'
import { availableChoices } from '../lib/conflicts'
import { formatBytes, formatUnix } from '../lib/format'
import { EmptyState } from './EmptyState'

type Props = {
  conflicts: Conflict[]
  choices: Record<string, ResolutionChoice>
  selectedPath: string | null
  preview: ConflictPreview | null
  previewBusy: boolean
  busy: boolean
  onSelect: (path: string) => void
  onChoose: (path: string, choice: ResolutionChoice) => void
  onResolve: () => void
}

export function Conflicts({
  conflicts,
  choices,
  selectedPath,
  preview,
  previewBusy,
  busy,
  onSelect,
  onChoose,
  onResolve,
}: Props) {
  return (
    <section className="section" aria-labelledby="conflicts-heading">
      <div className="section__heading">
        <h2 id="conflicts-heading">Conflicts</h2>
        {conflicts.length > 0 ? (
          <button
            className="button button--primary"
            data-testid="applyResolutionsButton"
            disabled={busy}
            onClick={onResolve}
            type="button"
          >
            Apply resolutions
          </button>
        ) : null}
      </div>

      {conflicts.length === 0 ? (
        <EmptyState>Conflicts appear here when a sync needs a decision.</EmptyState>
      ) : (
        <div className="conflict-list">
          {conflicts.map((conflict) => {
            const selected = selectedPath === conflict.path
            return (
              <div
                key={conflict.path}
                className={`conflict-card${selected ? ' conflict-card--selected' : ''}`}
              >
                <button
                  className="conflict-card__select"
                  data-testid="conflictRowTitle"
                  data-path={conflict.path}
                  aria-pressed={selected}
                  onClick={() => onSelect(conflict.path)}
                  type="button"
                >
                  <code className="conflict-card__path">{conflict.path}</code>
                </button>
                <dl className="conflict-meta">
                  <div>
                    <dt>This device</dt>
                    <dd>
                      {conflict.local.deleted
                        ? 'Deleted'
                        : `${formatBytes(conflict.local.size)} · ${formatUnix(conflict.local.modified)}`}
                    </dd>
                  </div>
                  <div>
                    <dt>Other device</dt>
                    <dd>
                      {conflict.remote.deleted
                        ? 'Deleted'
                        : `${formatBytes(conflict.remote.size)} · ${formatUnix(conflict.remote.modified)}`}
                    </dd>
                  </div>
                </dl>
                <div
                  className="choice-row"
                  role="group"
                  aria-label={`Resolution for ${conflict.path}`}
                >
                  {availableChoices(conflict).map(({ choice, label }) => (
                    <button
                      key={choice}
                      data-testid="winnerPicker"
                      data-choice={choice}
                      className={choices[conflict.path] === choice ? 'is-selected' : ''}
                      aria-pressed={choices[conflict.path] === choice}
                      onClick={() => onChoose(conflict.path, choice)}
                      type="button"
                    >
                      {label}
                    </button>
                  ))}
                </div>
                {selected && preview ? (
                  <div className="preview">
                    <div className="preview__header">
                      <span>{previewBusy ? 'Refreshing preview…' : 'Read-only preview'}</span>
                    </div>
                    <div className="preview-columns">
                      <PreviewColumn
                        title="This device"
                        deleted={preview.local_deleted}
                        content={preview.local_text}
                      />
                      <PreviewColumn
                        title="Other device"
                        deleted={preview.remote_deleted}
                        content={preview.remote_text}
                      />
                    </div>
                  </div>
                ) : null}
              </div>
            )
          })}
        </div>
      )}
    </section>
  )
}

function PreviewColumn({
  title,
  deleted,
  content,
}: {
  title: string
  deleted: boolean
  content: string
}) {
  return (
    <div className="preview-column">
      <h3>{title}</h3>
      {deleted ? (
        <EmptyState>Deleted in this version.</EmptyState>
      ) : (
        <pre>{content || 'Empty file.'}</pre>
      )}
    </div>
  )
}
