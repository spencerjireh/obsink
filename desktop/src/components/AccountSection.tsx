import { useEffect, useRef, useState } from 'react'
import type { AccountState, AuthCapabilities, InviteInfo } from '../types'
import { usageLine } from '../lib/format'
import { DEFAULT_SERVER_URL } from '../lib/server-url'
import { ConfirmForm } from './ConfirmForm'
import { DeviceList } from './DeviceList'
import { EmptyState } from './EmptyState'
import { InviteList } from './InviteList'

type Props = {
  serverUrl: string
  account: AccountState | null
  authEmail: string
  authCode: string
  inviteCode: string
  codeSent: boolean
  invites: InviteInfo[]
  busy: boolean
  capabilities: AuthCapabilities | null
  // Show the invite field: the server needs one for new accounts, or it
  // just refused a sign-up without one.
  inviteRequired: boolean
  // Bumped when the field should take focus (after that refusal).
  inviteFocusAt: number
  onServerUrlChange: (value: string) => void
  onServerUrlBlur: () => void
  onAuthEmailChange: (value: string) => void
  onAuthCodeChange: (value: string) => void
  onInviteCodeChange: (value: string) => void
  onSendCode: () => void
  onVerifyCode: () => void
  onChangeEmail: () => void
  onCreateInvite: () => void
  onCopyInvite: (code: string) => void
  onRevokeDevice: (sessionId: string) => void
  onSignOut: () => void
  onDeleteAccount: () => Promise<boolean>
}

export function AccountSection({
  serverUrl,
  account,
  authEmail,
  authCode,
  inviteCode,
  codeSent,
  invites,
  busy,
  capabilities,
  inviteRequired,
  inviteFocusAt,
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
  onRevokeDevice,
  onSignOut,
  onDeleteAccount,
}: Props) {
  const [confirmingDelete, setConfirmingDelete] = useState(false)
  const noServer = serverUrl.trim() === DEFAULT_SERVER_URL
  const inviteRef = useRef<HTMLInputElement>(null)

  useEffect(() => {
    if (inviteFocusAt > 0) {
      inviteRef.current?.focus()
    }
  }, [inviteFocusAt])

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
                className="button button--danger"
                disabled={busy || confirmingDelete}
                onClick={() => setConfirmingDelete(true)}
                type="button"
              >
                Delete account
              </button>
            </span>
          </div>
          {confirmingDelete ? (
            <ConfirmForm
              title="Delete account"
              description={`This deletes your account, every vault it owns on ${serverUrl.trim()}, and every signed-in device. Vault folders on this device stay.`}
              expected={account.email ?? 'delete'}
              caseInsensitive
              confirmLabel="Delete account"
              busy={busy}
              onConfirm={onDeleteAccount}
              onCancel={() => setConfirmingDelete(false)}
            />
          ) : null}
          <DeviceList
            devices={account.devices}
            busy={busy}
            onSignOut={onSignOut}
            onRevoke={onRevokeDevice}
          />
          <InviteList invites={invites} busy={busy} onCopy={onCopyInvite} />
        </>
      ) : capabilities && !capabilities.email ? (
        <EmptyState>This server has no email sign-in.</EmptyState>
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
          {inviteRequired ? (
            <label>
              <span>Invite code</span>
              <input
                ref={inviteRef}
                className="mono"
                autoCapitalize="characters"
                placeholder="new accounts only"
                value={inviteCode}
                onChange={(event) => onInviteCodeChange(event.target.value)}
              />
            </label>
          ) : null}
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
