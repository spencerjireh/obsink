import type { VaultStateInfo } from '../../types'
import type { Account } from '../../hooks/useAccount'
import { useBackend } from '../../backend'
import { useSyncRunner } from '../../hooks/useSyncRunner'
import { SESSION_EXPIRED } from '../../lib/errors'
import { formatRelative, phaseLabel, plural, vaultUsageLine } from '../../lib/format'
import { canSync, onThisDevice, remoteChanges, stateText, stateTone } from '../../lib/vault-state'
import { Conflicts } from '../../components/Conflicts'
import { InlineEdit } from '../../components/InlineEdit'
import { FailureNotice, Notice } from '../../components/Notices'
import { StateDot } from '../../components/StateDot'
import { VaultActions } from '../../components/VaultActions'
import { VaultActivity } from '../../components/VaultActivity'
import { VaultDevices } from '../../components/VaultDevices'

type Props = {
  info: VaultStateInfo
  account: Account
  message: string
  notify: (message: string) => void
  onError: (error: unknown) => void
  // The name or the folder changed; the list re-reads.
  onChanged: () => void
  onSignIn: () => void
  // Put this vault on this device (spec §15.1).
  onDownload: () => void
  // The vault is gone from this device (removed or deleted).
  onGone: () => void
}

// One vault: its state, the sync button, conflicts, and the manage section.
// Mounted with `key={info.id}` so the runner and any open confirmation
// belong to this vault only.
export function VaultPage({
  info,
  account,
  message,
  notify,
  onError,
  onChanged,
  onSignIn,
  onDownload,
  onGone,
}: Props) {
  const backend = useBackend()
  const runner = useSyncRunner(info.id, onError, notify)
  const busy = runner.busy || account.busy
  const here = onThisDevice(info)
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

  async function removal(action: () => Promise<void>, done: string): Promise<boolean> {
    try {
      await action()
      // Resetting the selection clears the message, so the notice comes after.
      onGone()
      notify(done)
      return true
    } catch (error) {
      onError(error)
      return false
    }
  }

  function openFolder() {
    backend.openVaultFolder(info.id).catch(onError)
  }

  // Spec §15.2 `Rename` and `Move folder`: in place, the list follows.
  async function change(action: () => Promise<void>, done: string): Promise<boolean> {
    try {
      await action()
      onChanged()
      notify(done)
      return true
    } catch (error) {
      onError(error)
      return false
    }
  }

  const rename = (name: string) =>
    change(() => backend.renameVault(info.id, name), `Renamed to ${name}.`)
  const title = (
    <InlineEdit
      value={info.name}
      label="Rename"
      fieldTestId="renameVaultField"
      buttonTestId="renameVaultButton"
      busy={busy || sessionExpired}
      onSave={rename}
    >
      <h1>{info.name}</h1>
    </InlineEdit>
  )

  // A vault the account owns that is not on this device: one action.
  if (info.state.kind === 'not_on_device') {
    return (
      <div className="vault-page">
        <header className="pane-header">
          <div className="pane-header__title">
            {title}
            <p className="state-line">
              <StateDot tone={stateTone(info)} />
              <span data-testid="vaultStateText">{stateText(info)}</span>
              {usage ? (
                <>
                  <span aria-hidden="true"> · </span>
                  <span className="mono">{vaultUsageLine(usage)}</span>
                </>
              ) : null}
            </p>
          </div>
          <button
            className="button button--primary"
            data-testid="downloadVaultButton"
            data-vault-id={info.id}
            disabled={busy || sessionExpired}
            onClick={onDownload}
            type="button"
          >
            Download
          </button>
        </header>
        {plainMessage ? <Notice>{plainMessage}</Notice> : null}
        <VaultDevices devices={info.devices} revision={info.revision} />
      </div>
    )
  }

  return (
    <div className="vault-page">
      <header className="pane-header">
        <div className="pane-header__title">
          {title}
          <p className="state-line">
            <StateDot tone={stateTone(info)} />
            <span data-testid="vaultStateText">{runner.busy ? 'Syncing…' : stateText(info)}</span>
            <span aria-hidden="true"> · </span>
            <span className="state-line__muted" data-testid="lastSyncedText">
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
            {backend.moveVaultFolder && here ? (
              <InlineEdit
                value={info.local_path ?? ''}
                label="Move folder"
                fieldTestId="moveFolderField"
                buttonTestId="moveFolderButton"
                busy={busy || sessionExpired}
                mono
                placeholder={backend.platform.folderPlaceholder}
                onSave={(path) =>
                  change(() => backend.moveVaultFolder!(info.id, path), `Moved ${info.name}.`)
                }
              >
                <code>{info.local_path ?? ''}</code>
                {backend.platform.canOpenFolder ? (
                  <button
                    className="button button--ghost button--small"
                    onClick={openFolder}
                    type="button"
                  >
                    Open folder
                  </button>
                ) : null}
              </InlineEdit>
            ) : (
              <>
                <code>{info.local_path ?? ''}</code>{' '}
                {backend.platform.canOpenFolder && here ? (
                  <button
                    className="button button--ghost button--small"
                    onClick={openFolder}
                    type="button"
                  >
                    Open folder
                  </button>
                ) : null}
              </>
            )}
          </p>
        </div>
        <button
          className="button button--primary"
          data-testid="syncButton"
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
        {info.state.kind === 'deleted_on_server' ? (
          <Notice kind="warning">
            This vault was deleted on the server. The folder on this device stays; remove the vault
            here when you are done with it.
          </Notice>
        ) : null}
        {info.state.kind === 'locked' ? (
          <Notice kind="warning">Unlock the account to sync this vault.</Notice>
        ) : null}
        {info.state.kind === 'needs_access' ? (
          <Notice
            kind="warning"
            action={
              backend.requestFolderAccess ? (
                <button
                  className="button button--ghost"
                  disabled={busy}
                  onClick={() => void backend.requestFolderAccess?.(info.id).catch(onError)}
                  type="button"
                >
                  Allow access
                </button>
              ) : undefined
            }
          >
            This browser needs permission to read and write the vault folder again.
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
          <Notice kind="warning" testId="staleBanner">
            {plural(stale, 'file')} changed on another device. Sync before editing.
          </Notice>
        ) : null}
        {sessionNotice}
        {plainMessage ? <Notice>{plainMessage}</Notice> : null}
        {runner.syncResult?.failures?.length ? (
          <FailureNotice failures={runner.syncResult.failures} />
        ) : null}
        {runner.syncResult?.checkpoint_error ? (
          <Notice kind="danger">
            <strong>Checkpoint failed:</strong> {runner.syncResult.checkpoint_error}. Sync again.
          </Notice>
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

      <VaultDevices devices={info.devices} revision={info.revision} />

      <VaultActivity vaultId={info.id} onError={onError} />

      <VaultActions
        vault={info}
        serverUrl={account.serverUrl}
        busy={busy}
        removeOnly={!syncable}
        onRemove={() =>
          removal(() => backend.removeVault(info.id), `Removed ${info.name} from this device.`)
        }
        onDeleteRemote={() =>
          removal(() => backend.deleteRemoteVault(info.id), `Deleted ${info.name} on the server.`)
        }
      />
    </div>
  )
}
