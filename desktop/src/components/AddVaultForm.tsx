import type { AddVaultForm as FormState, RemoteVault } from '../types'

type Props = {
  form: FormState
  remoteVaults: RemoteVault[] | null
  busy: boolean
  onFormChange: (patch: Partial<FormState>) => void
  onLoadRemoteVaults: () => void
  onSubmit: () => void
}

export function AddVaultForm({
  form,
  remoteVaults,
  busy,
  onFormChange,
  onLoadRemoteVaults,
  onSubmit,
}: Props) {
  return (
    <section className="section" id="setup-add-vault" aria-labelledby="add-vault-heading">
      <div className="section__heading">
        <h2 id="add-vault-heading">Add vault</h2>
        <span className="section__hint">
          {form.mode === 'create' ? 'Create a new remote vault' : 'Connect to an existing vault'}
        </span>
      </div>

      <div className="mode-toggle" role="group" aria-label="Mode">
        <button
          className={form.mode === 'connect' ? 'is-selected' : ''}
          aria-pressed={form.mode === 'connect'}
          onClick={() => onFormChange({ mode: 'connect' })}
          type="button"
        >
          Connect
        </button>
        <button
          className={form.mode === 'create' ? 'is-selected' : ''}
          aria-pressed={form.mode === 'create'}
          onClick={() => onFormChange({ mode: 'create' })}
          type="button"
        >
          Create
        </button>
      </div>

      <div className="form-grid">
        <label>
          <span>Local vault path</span>
          <input
            className="mono"
            autoCapitalize="off"
            autoCorrect="off"
            spellCheck={false}
            value={form.local_path}
            onChange={(event) => onFormChange({ local_path: event.target.value })}
          />
        </label>
        {form.mode === 'create' ? (
          <label>
            <span>Vault name</span>
            <input
              value={form.vault_name}
              onChange={(event) => onFormChange({ vault_name: event.target.value })}
            />
          </label>
        ) : (
          <label>
            <span>Vault</span>
            {remoteVaults ? (
              <select
                value={form.vault_id}
                onChange={(event) => onFormChange({ vault_id: event.target.value })}
              >
                {remoteVaults.map((vault) => (
                  <option key={vault.id} value={vault.id}>
                    {vault.name}
                  </option>
                ))}
              </select>
            ) : (
              <button
                className="button button--ghost"
                disabled={busy}
                onClick={onLoadRemoteVaults}
                type="button"
              >
                Load vaults
              </button>
            )}
          </label>
        )}
        <label>
          <span>Passphrase</span>
          <input
            type="password"
            autoComplete="off"
            value={form.passphrase}
            onChange={(event) => onFormChange({ passphrase: event.target.value })}
          />
        </label>
        <div className="form-grid__actions">
          <button
            className="button button--primary"
            disabled={busy}
            onClick={onSubmit}
            type="button"
          >
            {form.mode === 'create' ? 'Create vault' : 'Connect vault'}
          </button>
        </div>
      </div>
    </section>
  )
}
