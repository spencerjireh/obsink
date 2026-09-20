import type { VaultStateInfo } from '../../types'
import type { Account } from '../../hooks/useAccount'
import { useSyncRunner } from '../../hooks/useSyncRunner'
import { SESSION_EXPIRED } from '../../lib/errors'
import { formatRelative, phaseLabel, plural, vaultUsageLine } from '../../lib/format'
import { canSync, remoteChanges, stateText, stateTone } from '../../lib/vault-state'
import { call } from '../../lib/tauri'
import { Conflicts } from '../../components/Conflicts'
import { FailureNotice, Notice } from '../../components/Notices'
import { StateDot } from '../../components/StateDot'
import { VaultActions } from '../../components/VaultActions'

type Props = {
  info: VaultStateInfo
  account: Account
  message: string
  notify: (message: string) => void
  onError: (error: unknown) => void
  onSignIn: () => void
  // The vault is gone from this device (removed or deleted).
  onGone: () => void
}

// One vault: its state, the sync button, conflicts, and the manage section.
// Mounted with `key={info.id}` so the runner and any open confirmation
// belong to this vault only.
export function VaultPage({ info, account, message, notify, onError, onSignIn, onGone }: Props) {
  const runner = useSyncRunner(info.id, onError, notify)
  const busy = runner.busy || account.busy
  const syncable = canSync(info)
  const usage = account.vaultUsage(info.id)
  const stale = runner.conflicts.length > 0 ? 0 : remoteChanges(info)
  const sessionExpired = account.sessionExpired
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

  async function removal(command: string, done: string): Promise<boolean> {
    try {
      await call(command, { vaultId: info.id })
      notify(done)
      onGone()
      return true
    } catch (error) {
      onError(error)
      return false
    }
  }

  function openFolder() {
    call('open_vault_folder', { vaultId: info.id }).catch(onError)
  }

  return (
    <div className="vault-page">
      <header className="pane-header">
        <div className="pane-header__title">
          <h1>{info.name}</h1>
          <p className="state-line">
            <StateDot tone={stateTone(info)} />
            <span>{runner.busy ? 'Syncing…' : stateText(info)}</span>
            <span aria-hidden="true"> · </span>
            <span className="state-line__muted">
              {info.last_synced
                ? `Last synced ${formatRelative(info.last_synced)}`
                : 'Never synced'}
            </span>
            {usage ? (
              <>
                <span aria-hidden="true"> · </span>
                <span className="mono">{vaultUsageLine(usage)}</span>
              </>
            ) : null}
          </p>
          <p className="pane-header__meta">
            <code>{info.local_path}</code>{' '}
            <button
              className="button button--ghost button--small"
              onClick={openFolder}
              type="button"
            >
              Open folder
            </button>
          </p>
        </div>
        <button
          className="button button--primary"
          disabled={busy || sessionExpired || !syncable}
          onClick={() => void runner.sync()}
          type="button"
        >
          {runner.busy ? 'Working…' : 'Sync now'}
        </button>
      </header>

      <section className="section" aria-labelledby="status-heading">
        <div className="section__heading">
          <h2 id="status-heading">Status</h2>
        </div>
        {info.state.kind === 'foreign' ? (
          <Notice kind="warning">
            This vault is configured for <code>{info.server_url}</code>, not this build&apos;s
            server. Remove it from this device, then connect it again.
          </Notice>
        ) : null}
        {info.state.kind === 'no_key' ? (
          <Notice kind="warning">
            No key for this vault on this device. Remove it, then connect it again with the
            passphrase.
          </Notice>
        ) : null}
        {info.state.kind === 'error' && info.state.error_kind !== 'unauthorized' ? (
          <Notice kind={info.state.error_kind === 'network' ? 'warning' : 'danger'}>
            {info.state.error_kind === 'network'
              ? 'Could not reach the server. Check your connection.'
              : info.state.message}
          </Notice>
        ) : null}
        {runner.busy && runner.progress ? (
          <Notice>
            {phaseLabel(runner.progress.phase)}
            {runner.progress.path ? (
              <>
                {' · '}
                <code>{runner.progress.path}</code>
              </>
            ) : null}
            {runner.progress.total > 0
              ? ` (${runner.progress.current}/${runner.progress.total})`
              : ''}
          </Notice>
        ) : null}
        {stale > 0 ? (
          <Notice kind="warning">
            {plural(stale, 'file')} changed on another device. Sync before editing.
          </Notice>
        ) : null}
        {sessionNotice}
        {plainMessage ? <Notice>{plainMessage}</Notice> : null}
        {runner.syncResult?.failures?.length ? (
          <FailureNotice failures={runner.syncResult.failures} />
        ) : null}
        {!plainMessage && !sessionNotice && !stale && info.state.kind === 'up_to_date' ? (
          <p className="empty-state">Nothing to sync.</p>
        ) : null}
      </section>

      <Conflicts
        conflicts={runner.conflicts}
        choices={runner.choices}
        selectedPath={runner.selectedPath}
        preview={runner.preview}
        previewBusy={runner.previewBusy}
        busy={busy}
        onSelect={runner.select}
        onChoose={runner.choose}
        onResolve={() => void runner.resolve()}
      />

      <VaultActions
        vault={info}
        busy={busy}
        removeOnly={!syncable}
        onRemove={() => removal('remove_vault', `Removed ${info.name} from this device.`)}
        onDeleteRemote={() => removal('delete_remote_vault', `Deleted ${info.name} on the server.`)}
      />
    </div>
  )
}
