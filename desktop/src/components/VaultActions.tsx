import { useState } from 'react'
import type { LocalVault } from '../types'
import { ConfirmForm } from './ConfirmForm'

type Props = {
  vault: LocalVault
  busy: boolean
  onRemove: () => Promise<boolean>
  onDeleteRemote: () => Promise<boolean>
}

// Mounted with `key={vault.id}` so an open confirmation never outlives the
// vault it was asked about.
export function VaultActions({ vault, busy, onRemove, onDeleteRemote }: Props) {
  const [confirming, setConfirming] = useState<'remove' | 'delete' | null>(null)

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
        <button
          className="button button--danger"
          disabled={busy || confirming !== null}
          onClick={() => setConfirming('delete')}
          type="button"
        >
          Delete vault on server
        </button>
      </div>
      {confirming === 'remove' ? (
        <ConfirmForm
          title="Remove from this device"
          description="The vault stays on the server. The key is removed from the keychain, so connecting again needs the passphrase."
          confirmLabel="Remove from this device"
          busy={busy}
          onConfirm={onRemove}
          onCancel={() => setConfirming(null)}
        />
      ) : null}
      {confirming === 'delete' ? (
        <ConfirmForm
          title="Delete vault on server"
          description={`This deletes ${vault.name} and all of its files on ${vault.server_url}. The folder on this device stays.`}
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
