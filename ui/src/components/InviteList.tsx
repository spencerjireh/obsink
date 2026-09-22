import type { InviteInfo } from '../types'
import { formatUnix, inviteStatusLabel } from '../lib/format'
import { EmptyState } from './EmptyState'

type Props = {
  invites: InviteInfo[]
  busy: boolean
  onCopy: (code: string) => void
}

export function InviteList({ invites, busy, onCopy }: Props) {
  return (
    <div className="subsection" aria-labelledby="invites-heading">
      <h3 id="invites-heading">Invites</h3>
      {invites.length === 0 ? (
        <EmptyState>No invites yet.</EmptyState>
      ) : (
        <ul className="row-list">
          {invites.map((invite) => (
            <li className="row-item" data-testid="inviteRow" key={invite.code}>
              <span className="row-item__main">
                <code className="invite-code">{invite.code}</code>
                <span className={`tag tag--${invite.status}`}>
                  {inviteStatusLabel(invite.status)}
                </span>
                <span className="row-item__meta">
                  {invite.status === 'used' && invite.used_at !== null
                    ? `used ${formatUnix(invite.used_at)}`
                    : `expires ${formatUnix(invite.expires)}`}
                </span>
              </span>
              {invite.status === 'active' ? (
                <button
                  className="button button--ghost"
                  disabled={busy}
                  onClick={() => onCopy(invite.code)}
                  type="button"
                >
                  Copy
                </button>
              ) : null}
            </li>
          ))}
        </ul>
      )}
    </div>
  )
}
