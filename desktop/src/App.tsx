import { useEffect, useMemo, useRef, useState } from 'react'
import { invoke } from '@tauri-apps/api/core'
import { listen } from '@tauri-apps/api/event'

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
  failures: SyncFailure[]
}

type SyncFailure = {
  path: string
  kind: SyncAction['kind']
  error: string
  fatal: boolean
}

// Core `ProgressEvent` serializes (serde, externally-tagged) to this shape.
type SyncPhase = 'Downloading' | 'ResolvingConflicts' | 'Uploading'

type ProgressEvent =
  | { Phase: SyncPhase }
  | { FileStarted: { path: string; kind: SyncAction['kind']; index: number; total: number } }
  | { FileCompleted: { path: string; bytes: number } }
  | { FileFailed: { path: string; error: string } }
  | { Done: { uploaded: number; downloaded: number; failed: number } }

type Progress = {
  phase: SyncPhase
  current: number
  total: number
  path: string | null
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
  server_url: string
  local_path: string
  active: boolean
}

type SyncResponse = {
  completed_result: SyncResult | null
  pending_conflicts: Conflict[]
}

type ConflictPreview = {
  path: string
  local_text: string
  remote_text: string
  local_deleted: boolean
  remote_deleted: boolean
}

type AddVaultMode = 'create' | 'connect'
type ResolutionChoice = 'KeepLocal' | 'KeepRemote' | 'KeepBoth'

type UsageInfo = {
  total_bytes: number
  max_vault_bytes: number | null
  max_vaults: number | null
  vaults: { id: string; bytes: number }[]
}

type AccountState =
  | { kind: 'signed_out' }
  | {
      kind: 'account'
      user_id: string
      email: string | null
      devices: { session_id: string; device_name: string; current: boolean }[]
      usage: UsageInfo | null
    }

type InviteInfo = { code: string; expires: number }

function formatBytes(value: number): string {
  if (value < 1024) return `${value} B`
  const units = ['KB', 'MB', 'GB', 'TB']
  let size = value / 1024
  let unit = 0
  while (size >= 1024 && unit < units.length - 1) {
    size /= 1024
    unit += 1
  }
  return `${size.toFixed(size < 10 ? 1 : 0)} ${units[unit]}`
}

function usageLine(usage: UsageInfo | null): string {
  if (!usage) return ''
  const vaults = usage.max_vaults === null ? `${usage.vaults.length} vaults` : `${usage.vaults.length}/${usage.max_vaults} vaults`
  const cap = usage.max_vault_bytes === null ? '' : ` · ${formatBytes(usage.max_vault_bytes)} per vault`
  return ` · ${formatBytes(usage.total_bytes)} used · ${vaults}${cap}`
}

type RemoteVault = { id: string; name: string; created: number }

const SERVER_URL_KEY = 'obsink.serverUrl'

function rememberedServerUrl(): string {
  try {
    return window.localStorage.getItem(SERVER_URL_KEY) ?? 'https://'
  } catch {
    return 'https://'
  }
}

function rememberServerUrl(url: string) {
  try {
    window.localStorage.setItem(SERVER_URL_KEY, url)
  } catch {
    // Per-machine convenience only.
  }
}

const emptyForm = {
  mode: 'connect' as AddVaultMode,
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

function countRemoteChanges(diff: SyncResult): number {
  return diff.download.length + diff.conflicts.length
}

function phaseLabel(phase: SyncPhase): string {
  switch (phase) {
    case 'Downloading':
      return 'Downloading'
    case 'ResolvingConflicts':
      return 'Resolving conflicts'
    case 'Uploading':
      return 'Uploading'
  }
}

function App() {
  const [vaults, setVaults] = useState<LocalVault[]>([])
  const [status, setStatus] = useState<SyncStatus | null>(null)
  const [form, setForm] = useState(emptyForm)
  const [message, setMessage] = useState<string>('')
  const [busy, setBusy] = useState(false)
  const [syncResult, setSyncResult] = useState<SyncResult | null>(null)
  const [progress, setProgress] = useState<Progress | null>(null)
  const [conflicts, setConflicts] = useState<Conflict[]>([])
  const [choices, setChoices] = useState<Record<string, ResolutionChoice>>({})
  const [staleRemoteChanges, setStaleRemoteChanges] = useState(0)
  const [selectedConflictPath, setSelectedConflictPath] = useState<string | null>(null)
  const [conflictPreview, setConflictPreview] = useState<ConflictPreview | null>(null)
  const [previewBusy, setPreviewBusy] = useState(false)
  const handleSyncRef = useRef<() => Promise<void>>(async () => {})

  // One server per setup flow: enter the URL, sign in, then create or
  // connect a vault. The bearer lives in the keychain, keyed by server URL,
  // so sign-in happens once per machine.
  const [serverUrl, setServerUrl] = useState(rememberedServerUrl)
  const [account, setAccount] = useState<AccountState | null>(null)
  const [authEmail, setAuthEmail] = useState('')
  const [authCode, setAuthCode] = useState('')
  const [inviteCode, setInviteCode] = useState('')
  const [codeSent, setCodeSent] = useState(false)
  const [issuedInvite, setIssuedInvite] = useState<InviteInfo | null>(null)
  const [remoteVaults, setRemoteVaults] = useState<RemoteVault[] | null>(null)

  const currentServerUrl = serverUrl.trim()

  async function refreshAccount(url: string) {
    if (!url || url === 'https://') {
      setAccount(null)
      return
    }
    try {
      setAccount(await call<AccountState>('get_account', { serverUrl: url }))
    } catch (error) {
      setMessage(String(error))
    }
  }

  useEffect(() => {
    void refreshAccount(currentServerUrl)
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [])

  function handleServerUrlBlur() {
    setRemoteVaults(null)
    setCodeSent(false)
    void refreshAccount(currentServerUrl)
  }

  async function handleSendCode() {
    setBusy(true)
    setMessage('')
    try {
      const devCode = await call<string | null>('auth_email_start', { serverUrl: currentServerUrl, email: authEmail })
      setCodeSent(true)
      if (devCode) {
        setAuthCode(devCode)
        setMessage('Dev server returned the code inline.')
      } else {
        setMessage(`Sent a 6-digit code to ${authEmail}.`)
      }
    } catch (error) {
      setMessage(String(error))
    } finally {
      setBusy(false)
    }
  }

  async function handleVerifyCode() {
    setBusy(true)
    setMessage('')
    try {
      const next = await call<AccountState>('auth_email_verify', {
        serverUrl: currentServerUrl,
        email: authEmail,
        code: authCode,
        inviteCode: inviteCode.trim() || null,
      })
      setAccount(next)
      setCodeSent(false)
      setAuthCode('')
      setInviteCode('')
      rememberServerUrl(currentServerUrl)
      setMessage(next.kind === 'account' ? `Signed in as ${next.email ?? next.user_id}.` : 'Signed in.')
    } catch (error) {
      setMessage(String(error))
    } finally {
      setBusy(false)
    }
  }

  async function handleCreateInvite() {
    setBusy(true)
    setMessage('')
    try {
      const invite = await call<InviteInfo>('create_invite', { serverUrl: currentServerUrl })
      setIssuedInvite(invite)
    } catch (error) {
      setMessage(String(error))
    } finally {
      setBusy(false)
    }
  }

  async function handleCopyInvite() {
    if (!issuedInvite) return
    try {
      await navigator.clipboard.writeText(issuedInvite.code)
      setMessage('Invite code copied.')
    } catch (error) {
      setMessage(String(error))
    }
  }

  async function handleSignOut() {
    setBusy(true)
    setMessage('')
    try {
      await call('sign_out', { serverUrl: currentServerUrl })
      setAccount({ kind: 'signed_out' })
      setIssuedInvite(null)
      setRemoteVaults(null)
      setMessage(`Signed out of ${currentServerUrl}.`)
    } catch (error) {
      setMessage(String(error))
    } finally {
      setBusy(false)
    }
  }

  async function handleLoadRemoteVaults() {
    setBusy(true)
    setMessage('')
    try {
      const list = await call<RemoteVault[]>('list_remote_vaults', { serverUrl: currentServerUrl })
      setRemoteVaults(list)
      if (list.length === 0) {
        setMessage('No vaults on this server yet — switch to Create.')
      } else if (!form.vault_id) {
        setForm((current) => ({ ...current, vault_id: list[0].id }))
      }
    } catch (error) {
      setMessage(String(error))
    } finally {
      setBusy(false)
    }
  }

  const activeVault = useMemo(
    () => vaults.find((vault) => vault.active) ?? null,
    [vaults],
  )

  async function refresh() {
    const [nextVaults, nextStatus] = await Promise.all([
      call<LocalVault[]>('get_vaults'),
      call<SyncStatus>('get_status'),
    ])

    const nextActiveVault = nextVaults.find((vault) => vault.active) ?? null
    if (nextActiveVault) {
      const diff = await call<SyncResult>('get_manifest_diff', { vaultId: nextActiveVault.id })
      setStaleRemoteChanges(countRemoteChanges(diff))
    } else {
      setStaleRemoteChanges(0)
    }

    setVaults(nextVaults)
    setStatus(nextStatus)
  }

  useEffect(() => {
    refresh().catch((error) => setMessage(String(error)))
  }, [])

  useEffect(() => {
    if (conflicts.length === 0) {
      setSelectedConflictPath(null)
      setConflictPreview(null)
      return
    }

    if (!selectedConflictPath || !conflicts.some((conflict) => conflict.path === selectedConflictPath)) {
      setSelectedConflictPath(conflicts[0].path)
    }
  }, [conflicts, selectedConflictPath])

  useEffect(() => {
    if (!activeVault || !selectedConflictPath) {
      setConflictPreview(null)
      return
    }

    let cancelled = false
    setPreviewBusy(true)

    call<ConflictPreview>('get_conflict_preview', {
      vaultId: activeVault.id,
      path: selectedConflictPath,
    })
      .then((preview) => {
        if (!cancelled) {
          setConflictPreview(preview)
        }
      })
      .catch((error) => {
        if (!cancelled) {
          setMessage(String(error))
          setConflictPreview(null)
        }
      })
      .finally(() => {
        if (!cancelled) {
          setPreviewBusy(false)
        }
      })

    return () => {
      cancelled = true
    }
  }, [activeVault, selectedConflictPath])

  async function handleAddVault() {
    setBusy(true)
    setMessage('')

    try {
      const request = {
        ...form,
        server_url: currentServerUrl,
      }
      const saved = await call<LocalVault>('add_vault', { request })
      setMessage(`Configured ${saved.name}`)
      setForm(emptyForm)
      setRemoteVaults(null)
      await refresh()
      await refreshAccount(currentServerUrl)
    } catch (error) {
      setMessage(String(error))
    } finally {
      setBusy(false)
    }
  }

  async function handleSync() {
    setBusy(true)
    setMessage('')
    setProgress(null)

    try {
      const response = await call<SyncResponse>('sync_vault', {
        vaultId: activeVault?.id ?? null,
      })
      setSyncResult(response.completed_result)
      setConflicts(response.pending_conflicts)
      setSelectedConflictPath(response.pending_conflicts[0]?.path ?? null)
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

  // Keep the ref pointed at the latest handleSync so the tray listener,
  // registered once, always runs the current closure (with fresh activeVault).
  handleSyncRef.current = handleSync

  useEffect(() => {
    const unlisten = listen<ProgressEvent>('sync://progress', (event) => {
      const ev = event.payload
      if ('Phase' in ev) {
        setProgress({ phase: ev.Phase, current: 0, total: 0, path: null })
      } else if ('FileStarted' in ev) {
        setProgress((prev) => ({
          phase: prev?.phase ?? 'Uploading',
          current: ev.FileStarted.index + 1,
          total: ev.FileStarted.total,
          path: ev.FileStarted.path,
        }))
      } else if ('Done' in ev) {
        setProgress(null)
      }
    })
    return () => {
      void unlisten.then((dispose) => dispose())
    }
  }, [])

  useEffect(() => {
    const unlisten = listen('tray://sync-now', () => {
      void handleSyncRef.current()
    })
    return () => {
      void unlisten.then((dispose) => dispose())
    }
  }, [])

  async function handleResolveConflicts() {
    if (!activeVault) {
      return
    }

    setBusy(true)
    setMessage('')
    setProgress(null)

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
      setSelectedConflictPath(null)
      setConflictPreview(null)
      setMessage('Conflict resolutions applied.')
      await refresh()
    } catch (error) {
      setMessage(String(error))
    } finally {
      setBusy(false)
    }
  }

  async function handleSetActiveVault(vaultId: string) {
    setBusy(true)
    setMessage('')

    try {
      await call<LocalVault>('set_active_vault', { vaultId })
      setSyncResult(null)
      setConflicts([])
      setChoices({})
      setSelectedConflictPath(null)
      setConflictPreview(null)
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

        {staleRemoteChanges > 0 ? (
          <div className="notice notice--warning">
            {staleRemoteChanges} file{staleRemoteChanges === 1 ? '' : 's'} changed on another device.
            Sync before editing.
          </div>
        ) : null}
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

          {busy && progress ? (
            <div className="notice">
              {phaseLabel(progress.phase)}
              {progress.path ? ` · ${progress.path}` : ''}
              {progress.total > 0 ? ` (${progress.current}/${progress.total})` : ''}
            </div>
          ) : null}

          {message ? <div className="notice">{message}</div> : null}

          {syncResult?.failures?.length ? (
            <div className="notice notice--warning">
              {syncResult.failures.length} file{syncResult.failures.length === 1 ? '' : 's'} failed
              this sync:
              <ul className="failure-list">
                {syncResult.failures.map((failure) => (
                  <li key={failure.path}>
                    <span className={`tag tag--${failure.fatal ? 'fatal' : 'skipped'}`}>
                      {failure.fatal ? 'FATAL' : 'skipped'}
                    </span>{' '}
                    {failure.path}: {failure.error}
                  </li>
                ))}
              </ul>
            </div>
          ) : null}

          <div className="vault-list">
            {vaults.length === 0 ? <p>No vaults configured yet.</p> : null}
            {vaults.map((vault) => (
              <article key={vault.id} className={`vault-card${vault.active ? ' vault-card--active' : ''}`}>
                <header>
                  <h3>{vault.name}</h3>
                  <span>{vault.active ? 'Active' : vault.id}</span>
                </header>
                <p>{vault.server_url}</p>
                <code>{vault.local_path}</code>
                <div className="vault-card__actions">
                  <button
                    className="button button--ghost"
                    disabled={busy || vault.active}
                    onClick={() => handleSetActiveVault(vault.id)}
                    type="button"
                  >
                    {vault.active ? 'Current Vault' : 'Set Active'}
                  </button>
                </div>
              </article>
            ))}
          </div>
        </div>

        <div className="panel setup-panel">
          <div className="section-heading">
            <h2>Vault Setup</h2>
            <span>{form.mode === 'create' ? 'Create a new remote vault' : 'Connect to an existing vault'}</span>
          </div>

          <div className="form-grid">
            <label>
              <span>Server URL</span>
              <input
                value={serverUrl}
                onBlur={handleServerUrlBlur}
                onChange={(event) => setServerUrl(event.target.value)}
              />
            </label>
          </div>

          {account?.kind === 'account' ? (
            <>
              <div className="account-row">
                <span>
                  Signed in as <strong>{account.email ?? account.user_id}</strong>
                  {account.devices.length > 1 ? ` · ${account.devices.length} devices` : ''}
                  {usageLine(account.usage)}
                </span>
                <span className="choice-row">
                  <button className="button button--ghost" disabled={busy} onClick={handleCreateInvite} type="button">
                    Invite someone
                  </button>
                  <button className="button button--ghost" disabled={busy} onClick={handleSignOut} type="button">
                    Sign out
                  </button>
                </span>
              </div>
              {issuedInvite ? (
                <div className="invite-box">
                  <span>
                    Invite code <code>{issuedInvite.code}</code> · expires {formatUnix(issuedInvite.expires)}
                  </span>
                  <button className="button button--ghost" onClick={handleCopyInvite} type="button">
                    Copy
                  </button>
                </div>
              ) : null}
            </>
          ) : (
            <div className="form-grid">
              <label>
                <span>Email</span>
                <input
                  autoComplete="email"
                  disabled={codeSent}
                  value={authEmail}
                  onChange={(event) => setAuthEmail(event.target.value)}
                />
              </label>
              <label>
                <span>Invite code (new accounts only)</span>
                <input
                  autoCapitalize="characters"
                  placeholder="optional"
                  value={inviteCode}
                  onChange={(event) => setInviteCode(event.target.value)}
                />
              </label>
              {codeSent ? (
                <label>
                  <span>6-digit code</span>
                  <input inputMode="numeric" value={authCode} onChange={(event) => setAuthCode(event.target.value)} />
                </label>
              ) : null}
              <div className="choice-row">
                {codeSent ? (
                  <>
                    <button className="button" disabled={busy || authCode.trim().length !== 6} onClick={handleVerifyCode} type="button">
                      Verify and sign in
                    </button>
                    <button className="button button--ghost" disabled={busy} onClick={() => setCodeSent(false)} type="button">
                      Change email
                    </button>
                  </>
                ) : (
                  <button
                    className="button"
                    disabled={busy || !authEmail.includes('@') || currentServerUrl === 'https://'}
                    onClick={handleSendCode}
                    type="button"
                  >
                    Send sign-in code
                  </button>
                )}
              </div>
            </div>
          )}

          <div className="mode-toggle" aria-label="Mode">
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
                <span>Vault</span>
                {remoteVaults ? (
                  <select value={form.vault_id} onChange={(event) => setForm((current) => ({ ...current, vault_id: event.target.value }))}>
                    {remoteVaults.map((vault) => (
                      <option key={vault.id} value={vault.id}>
                        {vault.name}
                      </option>
                    ))}
                  </select>
                ) : (
                  <button className="button button--ghost" disabled={busy} onClick={handleLoadRemoteVaults} type="button">
                    Load vaults
                  </button>
                )}
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
          {selectedConflictPath && conflictPreview ? (
            <div className="preview-panel">
              <div className="preview-panel__header">
                <strong>{selectedConflictPath}</strong>
                <span>{previewBusy ? 'Refreshing preview...' : 'Read-only preview'}</span>
              </div>
              <div className="preview-columns">
                <PreviewColumn
                  title="This device"
                  deleted={conflictPreview.local_deleted}
                  content={conflictPreview.local_text}
                />
                <PreviewColumn
                  title="Other device"
                  deleted={conflictPreview.remote_deleted}
                  content={conflictPreview.remote_text}
                />
              </div>
            </div>
          ) : null}
          {conflicts.length === 0 ? (
            <p className="empty-state">Conflicts will appear here when sync pauses for review.</p>
          ) : (
            conflicts.map((conflict) => (
              <article
                key={conflict.path}
                className={`conflict-card${selectedConflictPath === conflict.path ? ' conflict-card--selected' : ''}`}
                onClick={() => setSelectedConflictPath(conflict.path)}
              >
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

function PreviewColumn({ title, deleted, content }: { title: string; deleted: boolean; content: string }) {
  return (
    <div className="preview-column">
      <h3>{title}</h3>
      {deleted ? <p className="empty-state">Deleted in this version.</p> : <pre>{content || 'Empty file.'}</pre>}
    </div>
  )
}

export default App
