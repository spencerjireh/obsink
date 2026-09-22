import { useState } from 'react'
import type { DeviceInfo, VaultStateInfo } from '../../types'
import type { Account } from '../../hooks/useAccount'
import { platformLabel } from '../../lib/devices'
import { formatRelative } from '../../lib/format'
import { ConfirmForm } from '../../components/ConfirmForm'
import { InlineEdit } from '../../components/InlineEdit'

type Props = {
  account: Account
  states: VaultStateInfo[]
  busy: boolean
}

// Spec §15.3: one row per device of the account. Signing out this device is
// the ordinary sign-out; signing out another device revokes it on the server
// (its folders and keys stay where they are).
export function DevicesTab({ account, states, busy }: Props) {
  const [revoking, setRevoking] = useState<DeviceInfo | null>(null)
  const devices = account.account?.kind === 'account' ? account.account.devices : []
  const names = new Map(states.map((s) => [s.id, s.name]))

  function vaultsLine(device: DeviceInfo): string {
    if (device.vault_ids.length === 0) return 'No vaults on this device.'
    return device.vault_ids.map((id) => names.get(id) ?? id).join(', ')
  }

  return (
    <section className="section" aria-labelledby="devices-heading">
      <div className="section__heading">
        <h2 id="devices-heading">Devices</h2>
        <span className="section__hint">
          Every device signed in to this account. Signing one out keeps its folders.
        </span>
      </div>
      <ul className="row-list">
        {devices.map((device) => (
          <li
            className="row-item row-item--stacked"
            data-testid="deviceRow"
            data-device-id={device.id}
            key={device.id}
          >
            <span className="row-item__main">
              <span className="tag">{platformLabel(device.platform)}</span>
              <InlineEdit
                value={device.name}
                label="Rename"
                fieldTestId="deviceRenameField"
                buttonTestId="deviceRenameButton"
                busy={busy}
                onSave={(name) => account.renameDevice(device.id, name)}
              >
                <span className="row-item__name">{device.name}</span>
              </InlineEdit>
              {device.current ? <span className="tag">This device</span> : null}
              <span className="row-item__meta">
                Last seen {formatRelative(device.last_seen).toLowerCase()}
              </span>
              <span className="row-item__meta row-item__line">{vaultsLine(device)}</span>
            </span>
            <button
              className="button button--ghost"
              data-testid="deviceSignOutButton"
              disabled={busy || revoking !== null}
              onClick={() => (device.current ? void account.signOut() : setRevoking(device))}
              type="button"
            >
              Sign out
            </button>
          </li>
        ))}
      </ul>
      {revoking ? (
        <ConfirmForm
          title={`Sign out ${revoking.name}`}
          description={`${revoking.name} is signed out on its next request. Its folders stay.`}
          confirmLabel="Sign out"
          busy={busy}
          onConfirm={() => account.revokeDevice(revoking.id)}
          onCancel={() => setRevoking(null)}
        />
      ) : null}
    </section>
  )
}
