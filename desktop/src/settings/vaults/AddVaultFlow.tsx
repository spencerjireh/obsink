import { useEffect, useState } from 'react'
import type { AddVaultMode, LocalVault, RemoteVault } from '../../types'
import type { Account } from '../../hooks/useAccount'
import { call } from '../../lib/tauri'
import { Notice } from '../../components/Notices'
import { SignInForm } from '../account/SignInForm'

type Step = 'sign-in' | 'choose' | 'folder' | 'passphrase' | 'done'

const STEPS: { id: Step; label: string }[] = [
  { id: 'sign-in', label: 'Sign in' },
  { id: 'choose', label: 'Choose vault' },
  { id: 'folder', label: 'Folder' },
  { id: 'passphrase', label: 'Passphrase' },
]

type Props = {
  account: Account
  message: string
  notify: (message: string) => void
  onError: (error: unknown) => void
  // Called with the new vault once it is configured, and on Done/Cancel.
  onAdded: (vault: LocalVault) => void
  onClose: () => void
}

// Adding a vault, one question at a time: sign in (only when signed out),
// pick or create the vault, choose the folder, set the passphrase. Ends on a
// card that says where the folder is.
export function AddVaultFlow({ account, message, notify, onError, onAdded, onClose }: Props) {
  const [step, setStep] = useState<Step>(account.signedIn ? 'choose' : 'sign-in')
  const [mode, setMode] = useState<AddVaultMode>('connect')
  const [remoteVaults, setRemoteVaults] = useState<RemoteVault[] | null>(null)
  const [vaultId, setVaultId] = useState('')
  const [vaultName, setVaultName] = useState('')
  const [localPath, setLocalPath] = useState('')
  const [passphrase, setPassphrase] = useState('')
  const [busy, setBusy] = useState(false)
  const [added, setAdded] = useState<LocalVault | null>(null)

  // Signing in moves past the first step on its own.
  useEffect(() => {
    if (step === 'sign-in' && account.signedIn) {
      setStep('choose')
    }
  }, [step, account.signedIn])

  // The vault list loads when the step opens; an empty server means Create.
  useEffect(() => {
    if (step !== 'choose' || remoteVaults !== null) return
    let cancelled = false
    call<RemoteVault[]>('list_remote_vaults')
      .then((list) => {
        if (cancelled) return
        setRemoteVaults(list)
        if (list.length === 0) {
          setMode('create')
        } else if (!vaultId) {
          setVaultId(list[0].id)
        }
      })
      .catch((error) => {
        if (!cancelled) {
          setRemoteVaults([])
          onError(error)
        }
      })
    return () => {
      cancelled = true
    }
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [step, remoteVaults])

  const stepIndex = STEPS.findIndex((entry) => entry.id === step)
  const visibleSteps = account.signedIn && step !== 'sign-in' ? STEPS.slice(1) : STEPS

  const valid =
    step === 'choose'
      ? mode === 'create'
        ? vaultName.trim().length > 0
        : vaultId.length > 0
      : step === 'folder'
        ? localPath.trim().length > 0
        : step === 'passphrase'
          ? passphrase.length > 0
          : false

  function back() {
    if (step === 'folder') setStep('choose')
    else if (step === 'passphrase') setStep('folder')
  }

  async function next() {
    if (!valid) return
    if (step === 'choose') setStep('folder')
    else if (step === 'folder') setStep('passphrase')
    else if (step === 'passphrase') await submit()
  }

  async function submit() {
    setBusy(true)
    try {
      const request = {
        mode,
        local_path: localPath.trim(),
        vault_name: vaultName.trim(),
        vault_id: vaultId,
        passphrase,
      }
      const saved = await call<LocalVault>('add_vault', { request })
      setAdded(saved)
      setPassphrase('')
      setStep('done')
      notify(`Added ${saved.name}.`)
      onAdded(saved)
    } catch (error) {
      onError(error)
    } finally {
      setBusy(false)
    }
  }

  function openFolder() {
    if (!added) return
    call('open_vault_folder', { vaultId: added.id }).catch(onError)
  }

  if (step === 'done' && added) {
    return (
      <div className="vault-page">
        <header className="pane-header">
          <div className="pane-header__title">
            <h1>Added {added.name}</h1>
          </div>
        </header>
        <div className="card">
          <p>
            Open this folder in Obsidian as a vault. Sync runs from here when you press{' '}
            <strong>Sync now</strong>.
          </p>
          <p>
            <code>{added.local_path}</code>
          </p>
          <div className="choice-row">
            <button className="button button--ghost" onClick={openFolder} type="button">
              Open folder
            </button>
            <button className="button button--primary" onClick={onClose} type="button">
              Done
            </button>
          </div>
        </div>
      </div>
    )
  }

  return (
    <div className="vault-page">
      <header className="pane-header">
        <div className="pane-header__title">
          <h1>Add vault</h1>
        </div>
        <button className="button button--ghost" disabled={busy} onClick={onClose} type="button">
          Cancel
        </button>
      </header>

      <ol className="stepper">
        {visibleSteps.map((entry) => {
          const index = STEPS.findIndex((candidate) => candidate.id === entry.id)
          const status = index < stepIndex ? 'done' : index === stepIndex ? 'current' : 'todo'
          return (
            <li
              key={entry.id}
              className={`step step--${status}`}
              aria-current={status === 'current' ? 'step' : undefined}
            >
              {entry.label}
            </li>
          )
        })}
      </ol>

      {message ? <Notice>{message}</Notice> : null}

      {step === 'sign-in' ? (
        <section className="section" aria-labelledby="step-heading">
          <div className="section__heading">
            <h2 id="step-heading">Sign in</h2>
            <span className="section__hint mono">{account.serverUrl}</span>
          </div>
          <SignInForm account={account} busy={busy || account.busy} />
        </section>
      ) : null}

      {step === 'choose' ? (
        <form
          className="section"
          aria-labelledby="step-heading"
          onSubmit={(event) => {
            event.preventDefault()
            void next()
          }}
        >
          <div className="section__heading">
            <h2 id="step-heading">Choose vault</h2>
            <span className="section__hint">
              {mode === 'create'
                ? 'Create a new vault on the server'
                : 'Connect to an existing vault'}
            </span>
          </div>
          <div className="mode-toggle" role="group" aria-label="Mode">
            <button
              className={mode === 'connect' ? 'is-selected' : ''}
              aria-pressed={mode === 'connect'}
              disabled={remoteVaults !== null && remoteVaults.length === 0}
              onClick={() => setMode('connect')}
              type="button"
            >
              Connect
            </button>
            <button
              className={mode === 'create' ? 'is-selected' : ''}
              aria-pressed={mode === 'create'}
              onClick={() => setMode('create')}
              type="button"
            >
              Create
            </button>
          </div>
          <div className="form-grid">
            {mode === 'create' ? (
              <label>
                <span>Vault name</span>
                <input
                  autoFocus
                  value={vaultName}
                  onChange={(event) => setVaultName(event.target.value)}
                />
              </label>
            ) : (
              <label>
                <span>Vault</span>
                {remoteVaults === null ? (
                  <span className="empty-state">Loading vaults…</span>
                ) : remoteVaults.length === 0 ? (
                  <span className="empty-state">No vaults on this server yet. Create one.</span>
                ) : (
                  <select value={vaultId} onChange={(event) => setVaultId(event.target.value)}>
                    {remoteVaults.map((vault) => (
                      <option key={vault.id} value={vault.id}>
                        {vault.name}
                      </option>
                    ))}
                  </select>
                )}
              </label>
            )}
            <div className="choice-row form-grid__actions">
              <button className="button button--primary" disabled={!valid} type="submit">
                Next
              </button>
            </div>
          </div>
        </form>
      ) : null}

      {step === 'folder' ? (
        <form
          className="section"
          aria-labelledby="step-heading"
          onSubmit={(event) => {
            event.preventDefault()
            void next()
          }}
        >
          <div className="section__heading">
            <h2 id="step-heading">Folder</h2>
            <span className="section__hint">
              {mode === 'create'
                ? 'Where the vault lives on this Mac'
                : 'Where to put the vault on this Mac'}
            </span>
          </div>
          <div className="form-grid">
            <label className="form-grid__wide">
              <span>Local vault path</span>
              <input
                className="mono"
                autoCapitalize="off"
                autoCorrect="off"
                autoFocus
                spellCheck={false}
                placeholder="/Users/you/Documents/Notes"
                value={localPath}
                onChange={(event) => setLocalPath(event.target.value)}
              />
            </label>
            <div className="choice-row form-grid__actions">
              <button className="button button--ghost" onClick={back} type="button">
                Back
              </button>
              <button className="button button--primary" disabled={!valid} type="submit">
                Next
              </button>
            </div>
          </div>
        </form>
      ) : null}

      {step === 'passphrase' ? (
        <form
          className="section"
          aria-labelledby="step-heading"
          onSubmit={(event) => {
            event.preventDefault()
            void next()
          }}
        >
          <div className="section__heading">
            <h2 id="step-heading">Passphrase</h2>
            <span className="section__hint">
              {mode === 'create'
                ? 'Encrypts the vault. There is no recovery if it is lost.'
                : 'The passphrase this vault was created with.'}
            </span>
          </div>
          <div className="form-grid">
            <label>
              <span>Passphrase</span>
              <input
                type="password"
                autoComplete="off"
                autoFocus
                value={passphrase}
                onChange={(event) => setPassphrase(event.target.value)}
              />
            </label>
            <div className="choice-row form-grid__actions">
              <button className="button button--ghost" disabled={busy} onClick={back} type="button">
                Back
              </button>
              <button className="button button--primary" disabled={busy || !valid} type="submit">
                {busy ? 'Working…' : mode === 'create' ? 'Create vault' : 'Connect vault'}
              </button>
            </div>
          </div>
        </form>
      ) : null}
    </div>
  )
}
