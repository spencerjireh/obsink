import { useEffect, useRef } from 'react'
import type { Account } from '../../hooks/useAccount'
import { EmptyState } from '../../components/EmptyState'

// Email + one-time code sign-in. The invite field appears only when the
// server needs one (or just refused a sign-up without one).
export function SignInForm({ account, busy }: { account: Account; busy: boolean }) {
  const { form, capabilities, inviteRequired, inviteFocusAt } = account
  const inviteRef = useRef<HTMLInputElement>(null)

  useEffect(() => {
    if (inviteFocusAt > 0) {
      inviteRef.current?.focus()
    }
  }, [inviteFocusAt])

  if (capabilities && !capabilities.email) {
    return <EmptyState>This server has no email sign-in.</EmptyState>
  }

  return (
    <form
      className="form-grid"
      onSubmit={(event) => {
        event.preventDefault()
        void (form.codeSent ? account.verifyCode() : account.sendCode())
      }}
    >
      <label>
        <span>Email</span>
        <input
          data-testid="emailField"
          autoComplete="email"
          disabled={form.codeSent}
          value={form.authEmail}
          onChange={(event) => account.setAuthEmail(event.target.value)}
        />
      </label>
      {inviteRequired ? (
        <label>
          <span>Invite code</span>
          <input
            data-testid="inviteField"
            ref={inviteRef}
            className="mono"
            autoCapitalize="characters"
            placeholder="new accounts only"
            value={form.inviteCode}
            onChange={(event) => account.setInviteCode(event.target.value)}
          />
        </label>
      ) : null}
      {form.codeSent ? (
        <label>
          <span>6-digit code</span>
          <input
            data-testid="codeField"
            className="mono"
            inputMode="numeric"
            autoFocus
            value={form.authCode}
            onChange={(event) => account.setAuthCode(event.target.value)}
          />
        </label>
      ) : null}
      <div className="choice-row form-grid__actions">
        {form.codeSent ? (
          <>
            <button
              className="button button--primary"
              disabled={busy || form.authCode.trim().length !== 6}
              data-testid="signInButton"
              type="submit"
            >
              Verify and sign in
            </button>
            <button
              className="button button--ghost"
              disabled={busy}
              onClick={account.changeEmail}
              type="button"
            >
              Change email
            </button>
          </>
        ) : (
          <button
            className="button button--primary"
            disabled={busy || !form.authEmail.includes('@')}
            data-testid="sendCodeButton"
            type="submit"
          >
            Send sign-in code
          </button>
        )}
      </div>
    </form>
  )
}
