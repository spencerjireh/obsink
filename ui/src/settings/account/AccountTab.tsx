import { useState } from 'react'
import type { Account } from '../../hooks/useAccount'
import { usageLine } from '../../lib/format'
import { ConfirmForm } from '../../components/ConfirmForm'
import { DeviceList } from '../../components/DeviceList'
import { InviteList } from '../../components/InviteList'
import { SignInForm } from './SignInForm'

export function AccountTab({ account, busy }: { account: Account; busy: boolean }) {
  const [confirmingDelete, setConfirmingDelete] = useState(false)
  const current = account.account

  if (current?.kind !== 'account') {
    return (
      <section className="section" aria-labelledby="account-heading">
        <div className="section__heading">
          <h2 id="account-heading">Sign in</h2>
          <span className="section__hint mono">{account.serverUrl}</span>
        </div>
        <SignInForm account={account} busy={busy} />
      </section>
    )
  }

  return (
    <section className="section" aria-labelledby="account-heading">
      <div className="section__heading">
        <h2 id="account-heading">Account</h2>
        <span className="section__hint mono">{account.serverUrl}</span>
      </div>
      <div className="account-row">
        <span>
          Signed in as <strong>{current.email ?? current.user_id}</strong>
          {usageLine(current.usage)}
        </span>
        <span className="choice-row">
          <button
            className="button button--ghost"
            disabled={busy}
            onClick={() => void account.createInvite()}
            type="button"
          >
            Invite someone
          </button>
          <button
            className="button button--danger"
            data-testid="deleteAccountButton"
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
          description={`This deletes your account, every vault it owns on ${account.serverUrl}, and every signed-in device. Vault folders on this device stay.`}
          expected={current.email ?? 'delete'}
          caseInsensitive
          confirmLabel="Delete account"
          busy={busy}
          onConfirm={account.deleteAccount}
          onCancel={() => setConfirmingDelete(false)}
        />
      ) : null}
      <DeviceList
        devices={current.devices}
        busy={busy}
        onSignOut={() => void account.signOut()}
        onRevoke={(sessionId) => void account.revokeDevice(sessionId)}
      />
      <InviteList invites={account.invites} busy={busy} onCopy={account.copyInvite} />
    </section>
  )
}
