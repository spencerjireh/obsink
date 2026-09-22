import type { DeviceInfo } from '../types'
import { platformLabel } from '../lib/devices'
import { formatRelative } from '../lib/format'

type Props = {
  devices: DeviceInfo[]
  busy: boolean
  onSignOut: () => void
  onRevoke: (deviceId: string) => void
}

// Every device of the account. Signing out the current row is the ordinary
// sign-out; any other row signs that device out for good on the server (its
// folders stay).
export function DeviceList({ devices, busy, onSignOut, onRevoke }: Props) {
  return (
    <div className="subsection" aria-labelledby="devices-heading">
      <h3 id="devices-heading">Devices</h3>
      <ul className="row-list">
        {devices.map((device) => (
          <li
            className="row-item"
            data-testid="deviceRow"
            data-device-id={device.id}
            key={device.id}
          >
            <span className="row-item__main">
              <span className="tag">{platformLabel(device.platform)}</span>
              <span>{device.name}</span>
              {device.current ? <span className="tag">This device</span> : null}
              <span className="row-item__meta">
                Last seen {formatRelative(device.last_seen).toLowerCase()}
                {device.vault_ids.length > 0
                  ? ` · ${device.vault_ids.length} vault${device.vault_ids.length === 1 ? '' : 's'}`
                  : ''}
              </span>
            </span>
            <button
              className="button button--ghost"
              data-testid="deviceSignOutButton"
              disabled={busy}
              onClick={() => (device.current ? onSignOut() : onRevoke(device.id))}
              type="button"
            >
              Sign out
            </button>
          </li>
        ))}
      </ul>
    </div>
  )
}
