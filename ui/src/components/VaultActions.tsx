import { useState } from 'react'
import { useBackend } from '../backend'
import { ConfirmForm } from './ConfirmForm'

type Props = {
  vault: { id: string; name: string; server_url: string }
  busy: boolean
  // A vault this device cannot talk to (another server, no key) can only be
  // removed here.
  removeOnly?: boolean
  onRemove: () => Promise<boolean>
  onDeleteRemote: () => Promise<boolean>
}

// Mounted with `key={vault.id}` so an open confirmation never outlives the
// vault it was asked about.
export function VaultActions({ vault, busy, removeOnly = false, onRemove, onDeleteRemote }: Props) {
  const { kind, keyStoreNoun } = useBackend().platform
  const [confirming, setConfirming] = useState<'remove' | 'delete' | null>(null)
  // The browser never keeps a key; the desktop app has one in the keychain.
  const removeConsequence =
    kind === 'web'
      ? 'The vault stays on the server. This browser forgets the folder, so connecting again needs the passphrase.'
      : `The vault stays on the server. The key is removed from ${keyStoreNoun}, so connecting again needs the passphrase.`
  const deleteConsequence =
    kind === 'web' ? 'The folder on this computer stays.' : 'The folder on this device stays.'

  return (
    <section className="section" aria-labelledby="manage-heading">
      <div className="section__heading">
        <h2 id="manage-heading">Manage vault</h2>
      </div>
      <div className="choice-row">
        <button
          className="button button--ghost"
          disabled={busy || confirming !== null}
          onClick={() => setConfirming('remove')}
          type="button"
        >
          Remove from this device
        </button>
        {removeOnly ? null : (
          <button
            className="button button--danger"
            disabled={busy || confirming !== null}
            onClick={() => setConfirming('delete')}
            type="button"
          >
            Delete vault on server
          </button>
        )}
      </div>
      {confirming === 'remove' ? (
        <ConfirmForm
          title="Remove from this device"
          description={removeConsequence}
          confirmLabel="Remove from this device"
          busy={busy}
          onConfirm={onRemove}
          onCancel={() => setConfirming(null)}
        />
      ) : null}
      {confirming === 'delete' ? (
        <ConfirmForm
          title="Delete vault on server"
          description={`This deletes ${vault.name} and all of its files on ${vault.server_url}. ${deleteConsequence}`}
          expected={vault.name}
          confirmLabel="Delete vault on server"
          busy={busy}
          onConfirm={onDeleteRemote}
          onCancel={() => setConfirming(null)}
        />
      ) : null}
    </section>
  )
}
