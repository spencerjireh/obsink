import type { VaultDevice } from '../types'
import { formatRelative } from '../lib/format'
import { platformLabel } from '../lib/devices'
import { EmptyState } from './EmptyState'

// Spec §15.2: every device that holds this vault and how far behind the
// server each one is. Read-only; sign-out lives with the account.
export function VaultDevices({ devices, revision }: { devices: VaultDevice[]; revision: number }) {
  return (
    <section className="section" aria-labelledby="vault-devices-heading">
      <div className="section__heading">
        <h2 id="vault-devices-heading">Devices</h2>
      </div>
      {devices.length === 0 ? (
        <EmptyState>No device holds this vault yet.</EmptyState>
      ) : (
        <ul className="row-list">
          {devices.map((device) => {
            const behind =
              device.last_revision === null ? null : Math.max(0, revision - device.last_revision)
            return (
              <li
                className="row-item"
                data-testid="vaultDeviceRow"
                data-device-id={device.id}
                key={device.id}
              >
                <span className="row-item__main">
                  <span className="tag">{platformLabel(device.platform)}</span>
                  <span>{device.name}</span>
                  <span className="row-item__meta">
                    {device.last_synced
                      ? `Last synced ${formatRelative(device.last_synced).toLowerCase()}`
                      : 'Never synced'}
                    {behind === null
                      ? ''
                      : behind === 0
                        ? ' · Up to date'
                        : ` · ${behind} revision${behind === 1 ? '' : 's'} behind`}
                  </span>
                </span>
              </li>
            )
          })}
        </ul>
      )}
    </section>
  )
}
