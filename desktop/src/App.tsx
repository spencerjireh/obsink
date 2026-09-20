import { useEffect, useMemo, useRef, useState } from 'react'
import { listen } from '@tauri-apps/api/event'
import type {
  AccountState,
  AddVaultForm,
  AuthCapabilities,
  Conflict,
  ConflictPreview,
  InviteInfo,
  LocalVault,
  Progress,
  ProgressEvent,
  RemoteVault,
  ResolutionChoice,
  SetupFocus,
  SetupSection,
  SyncResponse,
  SyncResult,
  SyncStatus,
  VaultUsage,
  View,
} from './types'
import { call } from './lib/tauri'
import { isInviteRequired, isUnauthorized, SESSION_EXPIRED, toCommandError } from './lib/errors'
import { emptyForm } from './lib/conflicts'
import { countRemoteChanges, plural } from './lib/format'
import { MainPane } from './components/MainPane'
import { SetupView } from './components/SetupView'
import { Sidebar } from './components/Sidebar'

function App() {
  const [vaults, setVaults] = useState<LocalVault[]>([])
  const [status, setStatus] = useState<SyncStatus | null>(null)
  const [form, setForm] = useState<AddVaultForm>(emptyForm)
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
  const [view, setView] = useState<View>('vault')
  // Set by any command that came back 401: the bearer is gone, and the main
  // pane offers a way back to sign-in.
  const [sessionExpired, setSessionExpired] = useState(false)
  const [setupFocus, setSetupFocus] = useState<SetupFocus | null>(null)
  const handleSyncRef = useRef<() => Promise<void>>(async () => {})
  // Mirrors `busy` for the tray listener, which only sees refs: a second
  // "Sync now" while a cycle runs must not start another one.
  const busyRef = useRef(false)
  const conflictsPendingRef = useRef(false)

  // The one server this build talks to (baked in; never edited here). The
  // bearer lives in the keychain, keyed by server URL, so sign-in happens
  // once per machine.
  const [serverUrl, setServerUrl] = useState('')
  const [account, setAccount] = useState<AccountState | null>(null)
  const [authEmail, setAuthEmail] = useState('')
  const [authCode, setAuthCode] = useState('')
  const [inviteCode, setInviteCode] = useState('')
  const [codeSent, setCodeSent] = useState(false)
  const [invites, setInvites] = useState<InviteInfo[]>([])
  // What `GET /` says about this server; null until the URL has been checked.
  const [capabilities, setCapabilities] = useState<AuthCapabilities | null>(null)
  // The server refused a sign-up without an invite: show the field even when
  // capabilities said none was needed (they can go stale).
  const [inviteForced, setInviteForced] = useState(false)
  const [inviteFocusAt, setInviteFocusAt] = useState(0)
  const [remoteVaults, setRemoteVaults] = useState<RemoteVault[] | null>(null)

  const currentServerUrl = serverUrl.trim()

  // Every command failure lands here. A 401 has already cost the bearer on
  // the Rust side, so the account is signed out from this point.
  function fail(error: unknown) {
    const failure = toCommandError(error)
    if (isUnauthorized(failure)) {
      setSessionExpired(true)
      setAccount({ kind: 'signed_out' })
      setMessage(SESSION_EXPIRED)
    } else {
      setMessage(failure.message)
    }
  }

  async function refreshAccount() {
    try {
      const next = await call<AccountState>('get_account')
      setAccount(next)
      if (next.kind === 'account') {
        setSessionExpired(false)
        await refreshInvites()
      } else {
        setInvites([])
      }
    } catch (error) {
      fail(error)
    }
  }

  async function refreshInvites() {
    try {
      setInvites(await call<InviteInfo[]>('list_invites'))
    } catch (error) {
      fail(error)
    }
  }

  async function refreshCapabilities() {
    try {
      setCapabilities(await call<AuthCapabilities>('get_auth_capabilities'))
    } catch (error) {
      setCapabilities(null)
      fail(error)
    }
  }

  useEffect(() => {
    void call<string>('get_server_url').then(setServerUrl)
    void refreshAccount()
    void refreshCapabilities()
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [])

  async function handleSendCode() {
    setBusy(true)
    setMessage('')
    try {
      const devCode = await call<string | null>('auth_email_start', { email: authEmail })
      setCodeSent(true)
      if (devCode) {
        setAuthCode(devCode)
        setMessage('Dev server returned the code inline.')
      } else {
        setMessage(`Sent a 6-digit code to ${authEmail}.`)
      }
    } catch (error) {
      fail(error)
    } finally {
      setBusy(false)
    }
  }

  async function handleVerifyCode() {
    setBusy(true)
    setMessage('')
    try {
      const next = await call<AccountState>('auth_email_verify', {
        email: authEmail,
        code: authCode,
        inviteCode: inviteCode.trim() || null,
      })
      setAccount(next)
      setSessionExpired(false)
      setCodeSent(false)
      setAuthCode('')
      setInviteCode('')
      void refreshInvites()
      setMessage(
        next.kind === 'account' ? `Signed in as ${next.email ?? next.user_id}.` : 'Signed in.',
      )
    } catch (error) {
      const failure = toCommandError(error)
      if (isInviteRequired(failure)) {
        setInviteForced(true)
        setInviteFocusAt(Date.now())
        setMessage('Enter the invite code you were given.')
      } else {
        fail(failure)
      }
    } finally {
      setBusy(false)
    }
  }

  async function handleCreateInvite() {
    setBusy(true)
    setMessage('')
    try {
      await call<InviteInfo>('create_invite')
      await refreshInvites()
      setMessage('Invite code created.')
    } catch (error) {
      fail(error)
    } finally {
      setBusy(false)
    }
  }

  async function handleCopyInvite(code: string) {
    try {
      await navigator.clipboard.writeText(code)
      setMessage('Invite code copied.')
    } catch (error) {
      fail(error)
    }
  }

  async function handleRevokeDevice(sessionId: string) {
    setBusy(true)
    setMessage('')
    try {
      setAccount(await call<AccountState>('revoke_session', { sessionId }))
      setMessage('Device signed out.')
    } catch (error) {
      fail(error)
    } finally {
      setBusy(false)
    }
  }

  async function handleSignOut() {
    setBusy(true)
    setMessage('')
    try {
      await call('sign_out')
      setAccount({ kind: 'signed_out' })
      setInvites([])
      setRemoteVaults(null)
      setMessage(`Signed out of ${currentServerUrl}.`)
    } catch (error) {
      fail(error)
    } finally {
      setBusy(false)
    }
  }

  // Returns whether it succeeded so the confirmation form knows to close.
  async function handleDeleteAccount(): Promise<boolean> {
    setBusy(true)
    setMessage('')
    try {
      await call('delete_account')
      setAccount({ kind: 'signed_out' })
      setInvites([])
      setRemoteVaults(null)
      setSyncResult(null)
      setConflicts([])
      setChoices({})
      await refresh()
      setMessage('Account deleted.')
      return true
    } catch (error) {
      fail(error)
      return false
    } finally {
      setBusy(false)
    }
  }

  async function handleLoadRemoteVaults() {
    setBusy(true)
    setMessage('')
    try {
      const list = await call<RemoteVault[]>('list_remote_vaults')
      setRemoteVaults(list)
      if (list.length === 0) {
        setMessage('No vaults on this server yet. Switch to Create.')
      } else if (!form.vault_id) {
        setForm((current) => ({ ...current, vault_id: list[0].id }))
      }
    } catch (error) {
      fail(error)
    } finally {
      setBusy(false)
    }
  }

  const activeVault = useMemo(() => vaults.find((vault) => vault.active) ?? null, [vaults])

  // Usage is known for the setup server's account only; a vault on another
  // server shows none.
  const activeVaultUsage = useMemo<VaultUsage | null>(() => {
    if (!activeVault || account?.kind !== 'account' || !account.usage) return null
    if (currentServerUrl !== activeVault.server_url) return null
    const entry = account.usage.vaults.find((vault) => vault.id === activeVault.id)
    return { bytes: entry?.bytes ?? 0, max: account.usage.max_vault_bytes }
  }, [activeVault, account, currentServerUrl])

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
    // With nothing configured there is nothing to show but setup.
    if (nextVaults.length === 0) {
      setView('setup')
    }
  }

  useEffect(() => {
    refresh().catch(fail)
  }, [])

  useEffect(() => {
    if (conflicts.length === 0) {
      setSelectedConflictPath(null)
      setConflictPreview(null)
      return
    }

    if (
      !selectedConflictPath ||
      !conflicts.some((conflict) => conflict.path === selectedConflictPath)
    ) {
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
          fail(error)
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
      }
      const saved = await call<LocalVault>('add_vault', { request })
      setMessage(`Configured ${saved.name}.`)
      setForm(emptyForm)
      setRemoteVaults(null)
      await refresh()
      await refreshAccount()
      setView('vault')
    } catch (error) {
      fail(error)
    } finally {
      setBusy(false)
    }
  }

  // Apply a sync/resolve response: late 409s arrive as `pending_conflicts`
  // next to a `completed_result`, so both paths share this.
  async function applySyncResponse(response: SyncResponse, doneMessage: string) {
    setSyncResult(response.completed_result)
    setConflicts(response.pending_conflicts)
    setSelectedConflictPath(response.pending_conflicts[0]?.path ?? null)
    setConflictPreview(null)
    setChoices(
      Object.fromEntries(
        response.pending_conflicts.map((conflict) => [conflict.path, 'KeepLocal']),
      ),
    )

    if (response.pending_conflicts.length > 0) {
      const count = response.pending_conflicts.length
      setMessage(count === 1 ? '1 conflict needs attention.' : `${count} conflicts need attention.`)
      return
    }
    const failures = response.completed_result?.failures.length ?? 0
    setMessage(failures === 0 ? doneMessage : `Sync finished with ${plural(failures, 'failure')}.`)
    await refresh()
    // Usage in the header moves with what was just uploaded.
    void refreshAccount()
  }

  async function handleSync() {
    if (busyRef.current) {
      return
    }
    busyRef.current = true
    setBusy(true)
    setMessage('')
    setProgress(null)

    try {
      const response = await call<SyncResponse>('sync_vault', {
        vaultId: activeVault?.id ?? null,
      })
      await applySyncResponse(response, 'Sync complete.')
    } catch (error) {
      fail(error)
    } finally {
      busyRef.current = false
      setBusy(false)
    }
  }

  // Keep the ref pointed at the latest handleSync so the tray listener,
  // registered once, always runs the current closure (with fresh activeVault).
  handleSyncRef.current = handleSync
  conflictsPendingRef.current = conflicts.length > 0

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
      // A fresh cycle would discard the choices made so far; the resolver
      // has to finish first.
      if (conflictsPendingRef.current) {
        return
      }
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

    if (busyRef.current) {
      return
    }
    busyRef.current = true
    setBusy(true)
    setMessage('')
    setProgress(null)

    try {
      const response = await call<SyncResponse>('resolve_conflict', {
        vaultId: activeVault.id,
        resolutions: conflicts.map((conflict) => ({
          path: conflict.path,
          choice: choices[conflict.path] ?? 'KeepLocal',
        })),
      })
      await applySyncResponse(response, 'Conflict resolutions applied.')
    } catch (error) {
      fail(error)
    } finally {
      busyRef.current = false
      setBusy(false)
    }
  }

  async function handleSetActiveVault(vaultId: string) {
    setBusy(true)
    setMessage('')

    try {
      await call<LocalVault>('set_active_vault', { vaultId })
      clearVaultState()
      await refresh()
    } catch (error) {
      fail(error)
    } finally {
      setBusy(false)
    }
  }

  function clearVaultState() {
    setSyncResult(null)
    setConflicts([])
    setChoices({})
    setSelectedConflictPath(null)
    setConflictPreview(null)
  }

  async function runVaultRemoval(command: string, vaultId: string, done: string): Promise<boolean> {
    setBusy(true)
    setMessage('')
    try {
      await call(command, { vaultId })
      clearVaultState()
      await refresh()
      await refreshAccount()
      setMessage(done)
      return true
    } catch (error) {
      fail(error)
      return false
    } finally {
      setBusy(false)
    }
  }

  function handleSelectVault(vault: LocalVault) {
    setView('vault')
    if (!vault.active) {
      void handleSetActiveVault(vault.id)
    }
  }

  function openSetup(section: SetupSection) {
    setSetupFocus({ section, at: Date.now() })
    setView('setup')
    // Devices and invites change from other devices; show the current list.
    void refreshAccount()
  }

  return (
    <div className="app">
      <Sidebar
        vaults={vaults}
        busy={busy}
        view={view}
        onSelectVault={handleSelectVault}
        onAddVault={() => openSetup('add-vault')}
        onAccount={() => openSetup('account')}
      />
      {view === 'setup' ? (
        <SetupView
          focus={setupFocus}
          message={message}
          canClose={vaults.length > 0}
          onClose={() => setView('vault')}
          account={{
            serverUrl,
            account,
            authEmail,
            authCode,
            inviteCode,
            codeSent,
            invites,
            busy,
            capabilities,
            inviteRequired: (capabilities?.invite_required ?? false) || inviteForced,
            inviteFocusAt,
            onAuthEmailChange: setAuthEmail,
            onAuthCodeChange: setAuthCode,
            onInviteCodeChange: setInviteCode,
            onSendCode: handleSendCode,
            onVerifyCode: handleVerifyCode,
            onChangeEmail: () => setCodeSent(false),
            onCreateInvite: handleCreateInvite,
            onCopyInvite: handleCopyInvite,
            onRevokeDevice: handleRevokeDevice,
            onSignOut: handleSignOut,
            onDeleteAccount: handleDeleteAccount,
          }}
          addVault={{
            form,
            remoteVaults,
            busy,
            onFormChange: (patch) => setForm((current) => ({ ...current, ...patch })),
            onLoadRemoteVaults: handleLoadRemoteVaults,
            onSubmit: handleAddVault,
          }}
        />
      ) : (
        <MainPane
          activeVault={activeVault}
          status={status}
          busy={busy}
          progress={progress}
          message={message}
          sessionExpired={sessionExpired}
          staleRemoteChanges={staleRemoteChanges}
          syncResult={syncResult}
          conflicts={conflicts}
          choices={choices}
          selectedConflictPath={selectedConflictPath}
          conflictPreview={conflictPreview}
          previewBusy={previewBusy}
          onSync={() => void handleSync()}
          onResolve={() => void handleResolveConflicts()}
          onSelectConflict={setSelectedConflictPath}
          onChoose={(path, choice) => setChoices((current) => ({ ...current, [path]: choice }))}
          vaultUsage={activeVaultUsage}
          onAddVault={() => openSetup('add-vault')}
          onSignIn={() => openSetup('account')}
          onRemoveVault={() =>
            activeVault
              ? runVaultRemoval(
                  'remove_vault',
                  activeVault.id,
                  `Removed ${activeVault.name} from this device.`,
                )
              : Promise.resolve(false)
          }
          onDeleteRemoteVault={() =>
            activeVault
              ? runVaultRemoval(
                  'delete_remote_vault',
                  activeVault.id,
                  `Deleted ${activeVault.name} on the server.`,
                )
              : Promise.resolve(false)
          }
        />
      )}
    </div>
  )
}

export default App
