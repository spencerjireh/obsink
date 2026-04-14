import { useEffect, useMemo, useState } from 'react'
import { invoke } from '@tauri-apps/api/core'

type SyncAction = {
  path: string
  kind: 'Upload' | 'Download' | 'DeleteLocal' | 'DeleteRemote'
}

type Conflict = {
  path: string
  local: { modified: number; size: number; deleted?: boolean }
  remote: { modified: number; size: number; deleted?: boolean }
}

type SyncResult = {
  upload: SyncAction[]
  download: SyncAction[]
  conflicts: Conflict[]
}

type SyncStatus = {
  active_vault_id: string | null
  configured_vaults: number
  pending_uploads: number
  pending_downloads: number
  pending_conflicts: number
  last_sync_manifest_path: string | null
}

type LocalVault = {
  id: string
  name: string
  worker_url: string
  local_path: string
  active: boolean
}

type SyncResponse = {
  completed_result: SyncResult | null
  pending_conflicts: Conflict[]
}

type AddVaultMode = 'create' | 'connect'
type ResolutionChoice = 'KeepLocal' | 'KeepRemote' | 'KeepBoth'

const emptyForm = {
  mode: 'connect' as AddVaultMode,
  worker_url: 'https://',
  api_key: '',
  local_path: '',
  vault_name: '',
  vault_id: '',
  passphrase: '',
}

async function call<T>(command: string, args?: Record<string, unknown>): Promise<T> {
  return invoke<T>(command, args)
}

function formatUnix(value: number): string {
  if (!value) {
    return 'Unknown'
  }

  return new Date(value * 1000).toLocaleString()
}

function App() {
  const [vaults, setVaults] = useState<LocalVault[]>([])
  const [status, setStatus] = useState<SyncStatus | null>(null)
  const [form, setForm] = useState(emptyForm)
  const [message, setMessage] = useState<string>('')
  const [busy, setBusy] = useState(false)
  const [syncResult, setSyncResult] = useState<SyncResult | null>(null)
  const [conflicts, setConflicts] = useState<Conflict[]>([])
  const [choices, setChoices] = useState<Record<string, ResolutionChoice>>({})

  const activeVault = useMemo(
    () => vaults.find((vault) => vault.active) ?? null,
    [vaults],
  )

  async function refresh() {
    const [nextVaults, nextStatus] = await Promise.all([
      call<LocalVault[]>('get_vaults'),
      call<SyncStatus>('get_status'),
    ])
    setVaults(nextVaults)
    setStatus(nextStatus)
  }

  useEffect(() => {
    refresh().catch((error) => setMessage(String(error)))
  }, [])

  async function handleAddVault() {
    setBusy(true)
    setMessage('')

    try {
      const saved = await call<LocalVault>('add_vault', { request: form })
      setMessage(`Configured ${saved.name}`)
      setForm(emptyForm)
      await refresh()
    } catch (error) {
      setMessage(String(error))
    } finally {
      setBusy(false)
    }
  }

  async function handleSync() {
    setBusy(true)
    setMessage('')

    try {
      const response = await call<SyncResponse>('sync_vault', {
        vaultId: activeVault?.id ?? null,
      })
      setSyncResult(response.completed_result)
      setConflicts(response.pending_conflicts)
      setChoices(
        Object.fromEntries(
          response.pending_conflicts.map((conflict) => [conflict.path, 'KeepLocal']),
        ),
      )

      if (response.pending_conflicts.length === 0) {
        setMessage('Sync complete.')
        await refresh()
      } else {
        setMessage(`${response.pending_conflicts.length} conflicts need attention.`)
      }
    } catch (error) {
      setMessage(String(error))
    } finally {
      setBusy(false)
    }
  }

  async function handleResolveConflicts() {
    if (!activeVault) {
      return
    }

    setBusy(true)
    setMessage('')

    try {
      const result = await call<SyncResult>('resolve_conflict', {
        vaultId: activeVault.id,
        resolutions: conflicts.map((conflict) => ({
          path: conflict.path,
          choice: choices[conflict.path] ?? 'KeepLocal',
        })),
      })
      setSyncResult(result)
      setConflicts([])
      setChoices({})
      setMessage('Conflict resolutions applied.')
      await refresh()
    } catch (error) {
      setMessage(String(error))
    } finally {
      setBusy(false)
    }
  }

  return (
    <main className="shell">
      <section className="hero panel">
        <div className="hero__eyebrow">Self-hosted encrypted Obsidian sync</div>
        <div className="hero__headline">
          <h1>ObSink</h1>
          <p>
            A desktop control room for vault sync, conflict triage, and setup without
            leaving your notes flow.
          </p>
        </div>
        <div className="hero__meta">
          <div>
            <span>Active vault</span>
            <strong>{activeVault?.name ?? 'None configured'}</strong>
          </div>
          <div>
            <span>Configured vaults</span>
            <strong>{status?.configured_vaults ?? 0}</strong>
          </div>
          <div>
            <span>Manifest</span>
            <strong>{status?.last_sync_manifest_path ?? 'Not synced yet'}</strong>
          </div>
        </div>
      </section>

      <section className="grid">
        <div className="panel status-panel">
          <div className="section-heading">
            <h2>Sync Deck</h2>
            <button className="button button--primary" disabled={busy || !activeVault} onClick={handleSync}>
              {busy ? 'Working...' : 'Sync Now'}
            </button>
          </div>

          <div className="status-strip">
            <article>
              <span>Uploads</span>
              <strong>{status?.pending_uploads ?? 0}</strong>
            </article>
            <article>
              <span>Downloads</span>
              <strong>{status?.pending_downloads ?? 0}</strong>
            </article>
            <article>
              <span>Conflicts</span>
              <strong>{status?.pending_conflicts ?? 0}</strong>
            </article>
          </div>

          {message ? <div className="notice">{message}</div> : null}

          <div className="vault-list">
            {vaults.length === 0 ? <p>No vaults configured yet.</p> : null}
            {vaults.map((vault) => (
              <article key={vault.id} className={`vault-card${vault.active ? ' vault-card--active' : ''}`}>
                <header>
                  <h3>{vault.name}</h3>
                  <span>{vault.active ? 'Active' : vault.id}</span>
                </header>
                <p>{vault.worker_url}</p>
                <code>{vault.local_path}</code>
              </article>
            ))}
          </div>
        </div>

        <div className="panel setup-panel">
          <div className="section-heading">
            <h2>Vault Setup</h2>
            <span>{form.mode === 'create' ? 'Create a new remote vault' : 'Connect to an existing vault'}</span>
          </div>

          <div className="mode-toggle">
            <button
              className={form.mode === 'connect' ? 'is-selected' : ''}
              onClick={() => setForm((current) => ({ ...current, mode: 'connect' }))}
              type="button"
            >
              Connect
            </button>
            <button
              className={form.mode === 'create' ? 'is-selected' : ''}
              onClick={() => setForm((current) => ({ ...current, mode: 'create' }))}
              type="button"
            >
              Create
            </button>
          </div>

          <div className="form-grid">
            <label>
              <span>Worker URL</span>
              <input value={form.worker_url} onChange={(event) => setForm((current) => ({ ...current, worker_url: event.target.value }))} />
            </label>
            <label>
              <span>API key</span>
              <input value={form.api_key} onChange={(event) => setForm((current) => ({ ...current, api_key: event.target.value }))} />
            </label>
            <label>
              <span>Local vault path</span>
              <input value={form.local_path} onChange={(event) => setForm((current) => ({ ...current, local_path: event.target.value }))} />
            </label>
            {form.mode === 'create' ? (
              <label>
                <span>Vault name</span>
                <input value={form.vault_name} onChange={(event) => setForm((current) => ({ ...current, vault_name: event.target.value }))} />
              </label>
            ) : (
              <label>
                <span>Vault ID</span>
                <input value={form.vault_id} onChange={(event) => setForm((current) => ({ ...current, vault_id: event.target.value }))} />
              </label>
            )}
            <label>
              <span>Passphrase</span>
              <input type="password" value={form.passphrase} onChange={(event) => setForm((current) => ({ ...current, passphrase: event.target.value }))} />
            </label>
          </div>

          <button className="button button--ghost" disabled={busy} onClick={handleAddVault}>
            Save Vault
          </button>
        </div>
      </section>

      <section className="grid grid--bottom">
        <div className="panel results-panel">
          <div className="section-heading">
            <h2>Last Result</h2>
            <span>{syncResult ? 'Latest sync summary' : 'No completed sync yet'}</span>
          </div>
          {syncResult ? (
            <div className="result-columns">
              <ResultColumn title="Uploaded" items={syncResult.upload} />
              <ResultColumn title="Downloaded" items={syncResult.download} />
            </div>
          ) : (
            <p className="empty-state">Run a sync to populate upload and download activity.</p>
          )}
        </div>

        <div className="panel conflicts-panel">
          <div className="section-heading">
            <h2>Conflict Resolver</h2>
            <button className="button button--primary" disabled={busy || conflicts.length === 0} onClick={handleResolveConflicts}>
              Apply Decisions
            </button>
          </div>
          {conflicts.length === 0 ? (
            <p className="empty-state">Conflicts will appear here when sync pauses for review.</p>
          ) : (
            conflicts.map((conflict) => (
              <article key={conflict.path} className="conflict-card">
                <header>
                  <h3>{conflict.path}</h3>
                  <span>
                    local {formatUnix(conflict.local.modified)} / remote {formatUnix(conflict.remote.modified)}
                  </span>
                </header>
                <div className="conflict-meta">
                  <div>Local size: {conflict.local.size} bytes</div>
                  <div>Remote size: {conflict.remote.size} bytes</div>
                </div>
                <div className="choice-row">
                  {(['KeepLocal', 'KeepRemote', 'KeepBoth'] as ResolutionChoice[]).map((choice) => (
                    <button
                      key={choice}
                      className={choices[conflict.path] === choice ? 'is-selected' : ''}
                      onClick={() => setChoices((current) => ({ ...current, [conflict.path]: choice }))}
                      type="button"
                    >
                      {choice}
                    </button>
                  ))}
                </div>
              </article>
            ))
          )}
        </div>
      </section>
    </main>
  )
}

function ResultColumn({ title, items }: { title: string; items: SyncAction[] }) {
  return (
    <div className="result-column">
      <h3>{title}</h3>
      {items.length === 0 ? <p className="empty-state">No entries.</p> : null}
      {items.map((item) => (
        <article key={`${title}-${item.path}-${item.kind}`} className="result-row">
          <strong>{item.path}</strong>
          <span>{item.kind}</span>
        </article>
      ))}
    </div>
  )
}

export default App
