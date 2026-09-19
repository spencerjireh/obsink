import type { AccountState, InviteInfo } from '../types'
import { formatUnix, usageLine } from '../lib/format'
import { DEFAULT_SERVER_URL } from '../lib/server-url'

type Props = {
  serverUrl: string
  account: AccountState | null
  authEmail: string
  authCode: string
  inviteCode: string
  codeSent: boolean
  issuedInvite: InviteInfo | null
  busy: boolean
  onServerUrlChange: (value: string) => void
  onServerUrlBlur: () => void
  onAuthEmailChange: (value: string) => void
  onAuthCodeChange: (value: string) => void
  onInviteCodeChange: (value: string) => void
  onSendCode: () => void
  onVerifyCode: () => void
  onChangeEmail: () => void
  onCreateInvite: () => void
  onCopyInvite: () => void
  onSignOut: () => void
}

export function AccountSection({
  serverUrl,
  account,
  authEmail,
  authCode,
  inviteCode,
  codeSent,
  issuedInvite,
  busy,
  onServerUrlChange,
  onServerUrlBlur,
  onAuthEmailChange,
  onAuthCodeChange,
  onInviteCodeChange,
  onSendCode,
  onVerifyCode,
  onChangeEmail,
  onCreateInvite,
  onCopyInvite,
  onSignOut,
}: Props) {
  const noServer = serverUrl.trim() === DEFAULT_SERVER_URL

  return (
    <section className="section" id="setup-account" aria-labelledby="account-heading">
      <div className="section__heading">
        <h2 id="account-heading">Account</h2>
      </div>

      <div className="form-grid">
        <label>
          <span>Server URL</span>
          <input
            className="mono"
            autoCapitalize="off"
            autoCorrect="off"
            spellCheck={false}
            value={serverUrl}
            onBlur={onServerUrlBlur}
            onChange={(event) => onServerUrlChange(event.target.value)}
          />
        </label>
      </div>

      {account?.kind === 'account' ? (
        <>
          <div className="account-row">
            <span>
              Signed in as <strong>{account.email ?? account.user_id}</strong>
              {account.devices.length > 1 ? ` · ${account.devices.length} devices` : ''}
              {usageLine(account.usage)}
            </span>
            <span className="choice-row">
              <button
                className="button button--ghost"
                disabled={busy}
                onClick={onCreateInvite}
                type="button"
              >
                Invite someone
              </button>
              <button
                className="button button--ghost"
                disabled={busy}
                onClick={onSignOut}
                type="button"
              >
                Sign out
              </button>
            </span>
          </div>
          {issuedInvite ? (
            <div className="invite-box">
              <span>
                Invite code <code>{issuedInvite.code}</code> · expires{' '}
                {formatUnix(issuedInvite.expires)}
              </span>
              <button className="button button--ghost" onClick={onCopyInvite} type="button">
                Copy
              </button>
            </div>
          ) : null}
        </>
      ) : (
        <div className="form-grid">
          <label>
            <span>Email</span>
            <input
              autoComplete="email"
              disabled={codeSent}
              value={authEmail}
              onChange={(event) => onAuthEmailChange(event.target.value)}
            />
          </label>
          <label>
            <span>Invite code (new accounts only)</span>
            <input
              className="mono"
              autoCapitalize="characters"
              placeholder="optional"
              value={inviteCode}
              onChange={(event) => onInviteCodeChange(event.target.value)}
            />
          </label>
          {codeSent ? (
            <label>
              <span>6-digit code</span>
              <input
                className="mono"
                inputMode="numeric"
                value={authCode}
                onChange={(event) => onAuthCodeChange(event.target.value)}
              />
            </label>
          ) : null}
          <div className="choice-row form-grid__actions">
            {codeSent ? (
              <>
                <button
                  className="button button--primary"
                  disabled={busy || authCode.trim().length !== 6}
                  onClick={onVerifyCode}
                  type="button"
                >
                  Verify and sign in
                </button>
                <button
                  className="button button--ghost"
                  disabled={busy}
                  onClick={onChangeEmail}
                  type="button"
                >
                  Change email
                </button>
              </>
            ) : (
              <button
                className="button button--primary"
                disabled={busy || !authEmail.includes('@') || noServer}
                onClick={onSendCode}
                type="button"
              >
                Send sign-in code
              </button>
            )}
          </div>
        </div>
      )}
    </section>
  )
}
