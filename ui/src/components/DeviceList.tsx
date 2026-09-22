import type { DeviceInfo } from '../types'
import { formatUnix } from '../lib/format'

type Props = {
  devices: DeviceInfo[]
  busy: boolean
  onSignOut: () => void
  onRevoke: (sessionId: string) => void
}

// Every session of the account. Signing out the current row is the ordinary
// sign-out; any other row revokes that device's session on the server.
export function DeviceList({ devices, busy, onSignOut, onRevoke }: Props) {
  return (
    <div className="subsection" aria-labelledby="devices-heading">
      <h3 id="devices-heading">Devices</h3>
      <ul className="row-list">
        {devices.map((device) => (
          <li className="row-item" data-testid="deviceRow" key={device.session_id}>
            <span className="row-item__main">
              <span>{device.device_name}</span>
              {device.current ? <span className="tag">This device</span> : null}
              <span className="row-item__meta">since {formatUnix(device.created)}</span>
            </span>
            <button
              className="button button--ghost"
              data-testid="deviceSignOutButton"
              disabled={busy}
              onClick={() => (device.current ? onSignOut() : onRevoke(device.session_id))}
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
