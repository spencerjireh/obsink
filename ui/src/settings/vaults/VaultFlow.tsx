import { useEffect, useState } from 'react'
import type { LocalVault } from '../../types'
import { useBackend } from '../../backend'
import { Notice } from '../../components/Notices'
import { FolderStep } from './FolderStep'

type Step = 'name' | 'folder' | 'done'

// Create vault (name, then folder) and Download (folder) in one stepper:
// spec §10.3. Both end on a card with the folder path. The passphrase is
// never asked here; the account is unlocked before either flow opens.
type Props = {
  flow: { kind: 'create' } | { kind: 'download'; vault_id: string; vault_name: string }
  message: string
  notify: (message: string) => void
  onError: (error: unknown) => void
  // Called with the vault once it is on this device, and on Done/Cancel.
  onAdded: (vault: LocalVault) => void
  onClose: () => void
}

export function VaultFlow({ flow, message, notify, onError, onAdded, onClose }: Props) {
  const backend = useBackend()
  const { folderPrompt, canOpenFolder } = backend.platform
  const create = flow.kind === 'create'
  const [step, setStep] = useState<Step>(create ? 'name' : 'folder')
  const [vaultName, setVaultName] = useState('')
  const [folder, setFolder] = useState({ local_path: '', folder_name: '' })
  const [busy, setBusy] = useState(false)
  const [added, setAdded] = useState<LocalVault | null>(null)
  const steps: { id: Step; label: string }[] = create
    ? [
        { id: 'name', label: 'Name' },
        { id: 'folder', label: 'Folder' },
      ]
    : [{ id: 'folder', label: 'Folder' }]
  const stepIndex = steps.findIndex((entry) => entry.id === step)
  const title = create
    ? 'Create vault'
    : `Download ${flow.kind === 'download' ? flow.vault_name : ''}`

  // A folder picked here but never turned into a vault is forgotten when the
  // flow closes (the backend ignores this once the vault exists).
  useEffect(() => {
    return () => {
      void backend.discardPickedFolder?.()
    }
  }, [backend])

  const valid =
    step === 'name'
      ? vaultName.trim().length > 0
      : step === 'folder'
        ? folder.local_path.trim().length > 0
        : false

  async function submit() {
    setBusy(true)
    try {
      const saved = create
        ? await backend.createVault({
            vault_name: vaultName.trim(),
            local_path: folder.local_path.trim(),
          })
        : await backend.downloadVault({
            vault_id: flow.kind === 'download' ? flow.vault_id : '',
            local_path: folder.local_path.trim(),
          })
      setAdded(saved)
      setStep('done')
      notify(create ? `Created ${saved.name}.` : `Downloaded ${saved.name}.`)
      onAdded(saved)
    } catch (error) {
      onError(error)
    } finally {
      setBusy(false)
    }
  }

  function next() {
    if (!valid) return
    if (step === 'name') setStep('folder')
    else if (step === 'folder') void submit()
  }

  if (step === 'done' && added) {
    return (
      <div className="vault-page">
        <header className="pane-header">
          <div className="pane-header__title">
            <h1>{create ? `Created ${added.name}` : `Downloaded ${added.name}`}</h1>
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
            {canOpenFolder ? (
              <button
                className="button button--ghost"
                onClick={() => backend.openVaultFolder(added.id).catch(onError)}
                type="button"
              >
                Open folder
              </button>
            ) : null}
            <button
              className="button button--primary"
              data-testid="addVaultDoneButton"
              onClick={onClose}
              type="button"
            >
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
          <h1>{title}</h1>
        </div>
        <button className="button button--ghost" disabled={busy} onClick={onClose} type="button">
          Cancel
        </button>
      </header>

      {steps.length > 1 ? (
        <ol className="stepper">
          {steps.map((entry, index) => {
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
      ) : null}

      {message ? <Notice>{message}</Notice> : null}

      <form
        className="section"
        aria-labelledby="step-heading"
        onSubmit={(event) => {
          event.preventDefault()
          next()
        }}
      >
        {step === 'name' ? (
          <>
            <div className="section__heading">
              <h2 id="step-heading">Name</h2>
              <span className="section__hint">A new vault on the server, for every device</span>
            </div>
            <div className="form-grid">
              <label>
                <span>Vault name</span>
                <input
                  data-testid="createVaultNameField"
                  autoFocus
                  value={vaultName}
                  onChange={(event) => setVaultName(event.target.value)}
                />
              </label>
            </div>
          </>
        ) : null}
        {step === 'folder' ? (
          <div className="form-grid">
            <FolderStep
              hint={create ? folderPrompt.create : folderPrompt.download}
              busy={busy}
              value={folder}
              onChange={setFolder}
              onError={onError}
            />
          </div>
        ) : null}
        <div className="choice-row form-grid__actions">
          {step === 'folder' && create ? (
            <button
              className="button button--ghost"
              disabled={busy}
              onClick={() => setStep('name')}
              type="button"
            >
              Back
            </button>
          ) : null}
          <button
            className="button button--primary"
            data-testid={
              step === 'name'
                ? 'addVaultNextButton'
                : create
                  ? 'createVaultSubmitButton'
                  : 'downloadVaultSubmitButton'
            }
            disabled={busy || !valid}
            type="submit"
          >
            {busy ? 'Working…' : step === 'name' ? 'Next' : create ? 'Create vault' : 'Download'}
          </button>
        </div>
      </form>
    </div>
  )
}
