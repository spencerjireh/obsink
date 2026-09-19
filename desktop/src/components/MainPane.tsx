import type {
  Conflict,
  ConflictPreview,
  LocalVault,
  Progress,
  ResolutionChoice,
  SyncResult,
  SyncStatus,
  VaultUsage,
} from '../types'
import { SESSION_EXPIRED } from '../lib/errors'
import { phaseLabel, plural, vaultUsageLine } from '../lib/format'
import { Conflicts } from './Conflicts'
import { LastResult } from './LastResult'
import { FailureNotice, Notice } from './Notices'
import { StatusCounts } from './StatusCounts'
import { VaultActions } from './VaultActions'

type Props = {
  activeVault: LocalVault | null
  status: SyncStatus | null
  busy: boolean
  progress: Progress | null
  message: string
  sessionExpired: boolean
  staleRemoteChanges: number
  syncResult: SyncResult | null
  conflicts: Conflict[]
  choices: Record<string, ResolutionChoice>
  selectedConflictPath: string | null
  conflictPreview: ConflictPreview | null
  previewBusy: boolean
  vaultUsage: VaultUsage | null
  onSync: () => void
  onResolve: () => void
  onSelectConflict: (path: string) => void
  onChoose: (path: string, choice: ResolutionChoice) => void
  onAddVault: () => void
  onSignIn: () => void
  onRemoveVault: () => Promise<boolean>
  onDeleteRemoteVault: () => Promise<boolean>
}

export function MainPane({
  activeVault,
  status,
  busy,
  progress,
  message,
  sessionExpired,
  staleRemoteChanges,
  syncResult,
  conflicts,
  choices,
  selectedConflictPath,
  conflictPreview,
  previewBusy,
  vaultUsage,
  onSync,
  onResolve,
  onSelectConflict,
  onChoose,
  onAddVault,
  onSignIn,
  onRemoveVault,
  onDeleteRemoteVault,
}: Props) {
  const sessionNotice = sessionExpired ? (
    <Notice
      kind="warning"
      action={
        <button className="button button--ghost" disabled={busy} onClick={onSignIn} type="button">
          Sign in
        </button>
      }
    >
      {SESSION_EXPIRED}
    </Notice>
  ) : null
  // The expiry notice carries the same text, so the plain one is redundant.
  const plainMessage = message && !(sessionExpired && message === SESSION_EXPIRED) ? message : ''

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
        {sessionNotice}
        {plainMessage ? <Notice>{plainMessage}</Notice> : null}
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
            {vaultUsage ? (
              <>
                <span aria-hidden="true"> · </span>
                <span className="mono">{vaultUsageLine(vaultUsage)}</span>
              </>
            ) : null}
          </p>
        </div>
        <button
          className="button button--primary"
          disabled={busy || sessionExpired}
          onClick={onSync}
          type="button"
        >
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
        {sessionNotice}
        {plainMessage ? <Notice>{plainMessage}</Notice> : null}
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

      <VaultActions
        key={activeVault.id}
        vault={activeVault}
        busy={busy}
        onRemove={onRemoveVault}
        onDeleteRemote={onDeleteRemoteVault}
      />
    </main>
  )
}
