import { useEffect, useRef, useState } from 'react'
import { useBackend } from '../backend'
import { useActivity } from '../hooks/useActivity'
import { useVaultStates } from '../hooks/useVaultStates'
import { toCommandError } from '../lib/errors'
import { canSync, globalLine } from '../lib/vault-state'
import { BrandMark } from '../components/BrandMark'
import { RecentList } from './RecentList'
import { VaultRow } from './VaultRow'

// The menu-bar popover: a glance at every vault, the last few things that
// happened, and one button that syncs everything. Anything that needs a
// decision opens the settings window.
export function PopoverApp() {
  const backend = useBackend()
  const [message, setMessage] = useState('')
  const [syncingId, setSyncingId] = useState<string | null>(null)
  const fail = (error: unknown) => setMessage(toCommandError(error).message)
  const { states, refresh } = useVaultStates(fail, ['popover://opened'])
  const { events } = useActivity(null, 8, fail)
  const busyRef = useRef(false)
  const statesRef = useRef(states)
  statesRef.current = states

  // Sync every vault that can be synced, one after another. A vault that
  // fails (or stops on conflicts) shows it in its row; the loop goes on.
  async function syncAll() {
    if (busyRef.current) return
    busyRef.current = true
    setMessage('')
    try {
      // A vault holding a plan for the resolver is left alone: a fresh
      // cycle would discard the decisions being made in settings.
      const targets = statesRef.current.filter(
        (info) =>
          canSync(info) &&
          info.state.kind !== 'syncing' &&
          !(info.state.kind === 'conflicts' && info.state.awaiting_resolution),
      )
      for (const info of targets) {
        setSyncingId(info.id)
        try {
          const response = await backend.syncVault(info.id)
          if (response.pending_conflicts.length > 0) {
            setMessage(`${info.name} needs a decision. Open settings to resolve it.`)
          }
        } catch (error) {
          // Recorded in the activity log by the backend; the row shows the state.
          setMessage(`${info.name}: ${toCommandError(error).message}`)
        }
      }
    } finally {
      setSyncingId(null)
      busyRef.current = false
      void refresh()
    }
  }
  const syncAllRef = useRef(syncAll)
  syncAllRef.current = syncAll

  useEffect(() => backend.on('tray://sync-now', () => void syncAllRef.current()), [backend])
  useEffect(() => backend.on('popover://opened', () => setMessage('')), [backend])

  const busy = syncingId !== null
  const anySyncable = states.some(canSync)
  const openSettings = (target?: Parameters<typeof backend.openSettings>[0]) =>
    void backend.openSettings(target).catch(fail)

  return (
    <div className="popover">
      <header className="popover__header">
        <BrandMark />
        <span className="popover__name">ObSink</span>
        <button
          className="icon-button"
          aria-label="Open settings"
          title="Settings"
          onClick={() => openSettings()}
          type="button"
        >
          <svg viewBox="0 0 16 16" width="14" height="14" aria-hidden="true" focusable="false">
            <circle cx="8" cy="8" r="2.2" fill="none" stroke="currentColor" strokeWidth="1.2" />
            <path
              d="M8 1.8v1.6M8 12.6v1.6M1.8 8h1.6M12.6 8h1.6M3.6 3.6l1.2 1.2M11.2 11.2l1.2 1.2M3.6 12.4l1.2-1.2M11.2 4.8l1.2-1.2"
              stroke="currentColor"
              strokeWidth="1.2"
              strokeLinecap="round"
            />
          </svg>
        </button>
      </header>
      <p className="popover__global">{busy ? 'Syncing…' : globalLine(states)}</p>

      <ul className="popover__vaults" aria-label="Vaults">
        {states.map((info) => (
          <VaultRow
            key={info.id}
            info={info}
            syncing={syncingId === info.id}
            onOpen={() => openSettings({ tab: 'vaults', vault_id: info.id })}
            onOpenFolder={
              backend.platform.canOpenFolder
                ? () => void backend.openVaultFolder(info.id).catch(fail)
                : undefined
            }
          />
        ))}
        {states.length === 0 ? (
          <li className="vault-row">
            <button
              className="vault-row__main"
              onClick={() => openSettings({ tab: 'vaults', add_vault: true })}
              type="button"
            >
              <span className="vault-row__name">Add vault</span>
            </button>
          </li>
        ) : null}
      </ul>

      {message ? (
        <p className="popover__message" role="status">
          {message}
        </p>
      ) : null}

      <RecentList events={events} states={states} />

      <footer className="popover__footer">
        <button
          className="button button--primary"
          disabled={busy || !anySyncable}
          onClick={() => void syncAll()}
          type="button"
        >
          {busy ? 'Working…' : 'Sync now'}
        </button>
        <button className="button button--ghost" onClick={() => openSettings()} type="button">
          Settings
        </button>
      </footer>
    </div>
  )
}
