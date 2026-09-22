import { useState } from 'react'
import type { Account } from '../../hooks/useAccount'
import { usageLine } from '../../lib/format'
import { ConfirmForm } from '../../components/ConfirmForm'
import { InviteList } from '../../components/InviteList'
import { ChangePassphraseForm } from './ChangePassphraseForm'
import { SignInForm } from './SignInForm'
import { UnlockForm } from './UnlockForm'

// Spec §15.4: the account. Signed out: the sign-in form. Locked: the unlock
// form above the rest. Unlocked: who, where, usage, the passphrase, invites,
// sign out and delete.
export function SettingsTab({ account, busy }: { account: Account; busy: boolean }) {
  const [confirmingDelete, setConfirmingDelete] = useState(false)
  const current = account.account

  if (!current || current.kind === 'signed_out') {
    return (
      <section className="section" aria-labelledby="signin-heading">
        <div className="section__heading">
          <h2 id="signin-heading">Sign in</h2>
          <span className="section__hint mono">{account.serverUrl}</span>
        </div>
        <SignInForm account={account} busy={busy} />
      </section>
    )
  }

  const unlocked = current.kind === 'account'

  return (
    <>
      {current.kind === 'locked' ? <UnlockForm account={account} busy={busy} /> : null}
      <section className="section" aria-labelledby="account-heading">
        <div className="section__heading">
          <h2 id="account-heading">Account</h2>
          <span className="section__hint mono">{account.serverUrl}</span>
        </div>
        <div className="account-row">
          <span data-testid="signedInAsText">
            Signed in as <strong>{current.email ?? current.user_id}</strong>
            {unlocked ? usageLine(current.usage) : ''}
          </span>
          <span className="choice-row">
            <button
              className="button button--ghost"
              data-testid="signOutButton"
              disabled={busy}
              onClick={() => void account.signOut()}
              type="button"
            >
              Sign out
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
      </section>
      {unlocked ? (
        <>
          <section className="section" aria-labelledby="passphrase-heading">
            <div className="section__heading">
              <h2 id="passphrase-heading">Passphrase</h2>
              <span className="section__hint">
                Changes it for every device. There is no recovery if it is lost.
              </span>
            </div>
            <ChangePassphraseForm account={account} busy={busy} />
          </section>
          <section className="section" aria-labelledby="invites-heading">
            <div className="section__heading">
              <h2 id="invites-heading">Invites</h2>
              <button
                className="button button--ghost"
                disabled={busy}
                onClick={() => void account.createInvite()}
                type="button"
              >
                Invite someone
              </button>
            </div>
            <InviteList invites={account.invites} busy={busy} onCopy={account.copyInvite} />
          </section>
        </>
      ) : null}
    </>
  )
}
