import type {
  Conflict,
  ConflictPreview,
  LocalVault,
  Progress,
  ResolutionChoice,
  SyncResult,
  SyncStatus,
} from '../types'
import { phaseLabel, plural } from '../lib/format'
import { Conflicts } from './Conflicts'
import { LastResult } from './LastResult'
import { FailureNotice, Notice } from './Notices'
import { StatusCounts } from './StatusCounts'

type Props = {
  activeVault: LocalVault | null
  status: SyncStatus | null
  busy: boolean
  progress: Progress | null
  message: string
  staleRemoteChanges: number
  syncResult: SyncResult | null
  conflicts: Conflict[]
  choices: Record<string, ResolutionChoice>
  selectedConflictPath: string | null
  conflictPreview: ConflictPreview | null
  previewBusy: boolean
  onSync: () => void
  onResolve: () => void
  onSelectConflict: (path: string) => void
  onChoose: (path: string, choice: ResolutionChoice) => void
  onAddVault: () => void
}

export function MainPane({
  activeVault,
  status,
  busy,
  progress,
  message,
  staleRemoteChanges,
  syncResult,
  conflicts,
  choices,
  selectedConflictPath,
  conflictPreview,
  previewBusy,
  onSync,
  onResolve,
  onSelectConflict,
  onChoose,
  onAddVault,
}: Props) {
  if (!activeVault) {
    return (
      <main className="main">
        <header className="pane-header">
          <div>
            <h1>No vault selected</h1>
            <p className="pane-header__meta">Add a vault to start syncing.</p>
          </div>
          <button className="button button--primary" onClick={onAddVault} type="button">
            Add vault
          </button>
        </header>
        {message ? <Notice>{message}</Notice> : null}
      </main>
    )
  }

  return (
    <main className="main">
      <header className="pane-header">
        <div className="pane-header__title">
          <h1>{activeVault.name}</h1>
          <p className="pane-header__meta">
            <code>{activeVault.local_path}</code>
            <span aria-hidden="true"> · </span>
            <code>{activeVault.server_url}</code>
          </p>
        </div>
        <button className="button button--primary" disabled={busy} onClick={onSync} type="button">
          {busy ? 'Working…' : 'Sync now'}
        </button>
      </header>

      <section className="section" aria-labelledby="status-heading">
        <div className="section__heading">
          <h2 id="status-heading">Status</h2>
        </div>
        <StatusCounts status={status} />
        {busy && progress ? (
          <Notice>
            {phaseLabel(progress.phase)}
            {progress.path ? (
              <>
                {' · '}
                <code>{progress.path}</code>
              </>
            ) : null}
            {progress.total > 0 ? ` (${progress.current}/${progress.total})` : ''}
          </Notice>
        ) : null}
        {staleRemoteChanges > 0 ? (
          <Notice kind="warning">
            {plural(staleRemoteChanges, 'file')} changed on another device. Sync before editing.
          </Notice>
        ) : null}
        {message ? <Notice>{message}</Notice> : null}
        {syncResult?.failures?.length ? <FailureNotice failures={syncResult.failures} /> : null}
      </section>

      <LastResult result={syncResult} />

      <Conflicts
        conflicts={conflicts}
        choices={choices}
        selectedPath={selectedConflictPath}
        preview={conflictPreview}
        previewBusy={previewBusy}
        busy={busy}
        onSelect={onSelectConflict}
        onChoose={onChoose}
        onResolve={onResolve}
      />
    </main>
  )
}
