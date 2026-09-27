import { useEffect, useState } from 'react'
import type { Account } from '../../hooks/useAccount'
import { Notice } from '../../components/Notices'

// Spec §6.1: the wrapped account key is the new exposure, so the passphrase
// has a floor.
export const MIN_PASSPHRASE_CHARS = 12

// Spec §12.3: how often the waiting device asks whether it was approved.
const APPROVAL_POLL_MS = 3000

// Spec §12.1, the step after sign-in. A new account sets its passphrase
// (twice). An existing one waits to be approved from an unlocked device
// (spec §12.3) and can enter the passphrase instead; a device that lost the
// first-set race goes straight to the field. The account key then lives
// where the platform keeps it.
export function UnlockForm({ account, busy }: { account: Account; busy: boolean }) {
  const [passphrase, setPassphrase] = useState('')
  const [again, setAgain] = useState('')
  const [mismatch, setMismatch] = useState(false)
  // `Use passphrase instead` was pressed.
  const [showPassphrase, setShowPassphrase] = useState(false)
  const locked = account.account?.kind === 'locked' ? account.account : null
  // After a lost race the account has a key: the form turns into Unlock.
  const setting = locked !== null && !locked.has_key
  // A passphrase exists: wait for approval, the field on request.
  const waiting = locked !== null && locked.has_key
  const passphraseVisible = setting || showPassphrase || account.raceNotice !== ''
  const tooShort = passphrase.length > 0 && passphrase.length < MIN_PASSPHRASE_CHARS
  const valid = setting
    ? passphrase.length >= MIN_PASSPHRASE_CHARS && again.length > 0
    : passphrase.length > 0

  // The poll loop: at once, then every few seconds, for as long as this
  // form shows a locked account with a passphrase set.
  const { pollApproval } = account
  useEffect(() => {
    if (!waiting) return
    void pollApproval()
    const timer = setInterval(() => void pollApproval(), APPROVAL_POLL_MS)
    return () => clearInterval(timer)
  }, [waiting, pollApproval])

  async function submit() {
    if (!valid || busy) return
    if (setting && passphrase !== again) {
      setMismatch(true)
      return
    }
    setMismatch(false)
    const ok = setting ? await account.setPassphrase(passphrase) : await account.unlock(passphrase)
    if (ok) {
      setPassphrase('')
      setAgain('')
    }
  }

  const hint = setting
    ? 'It unlocks every vault on every device. There is no recovery if it is lost.'
    : waiting
      ? 'Approve this device from one that is unlocked, or enter the passphrase.'
      : 'The account passphrase, set on your first device.'

  return (
    <section className="section" aria-labelledby="unlock-heading">
      <div className="section__heading">
        <h2 id="unlock-heading">{setting ? 'Set passphrase' : 'Unlock'}</h2>
        <span className="section__hint">{hint}</span>
      </div>
      {account.raceNotice ? <Notice kind="warning">{account.raceNotice}</Notice> : null}
      {waiting ? (
        <div className="form-grid">
          <code className="approval-fingerprint" data-testid="approvalFingerprintText">
            {account.approval?.fingerprint ?? ''}
          </code>
          <p className="section__hint" data-testid="approvalWaitingText" role="status">
            {account.approval
              ? 'Waiting for approval from another device.'
              : 'Requesting approval…'}
          </p>
          {passphraseVisible ? null : (
            <div className="choice-row form-grid__actions">
              <button
                className="button button--ghost"
                data-testid="usePassphraseButton"
                onClick={() => setShowPassphrase(true)}
                type="button"
              >
                Use passphrase instead
              </button>
            </div>
          )}
        </div>
      ) : null}
      {passphraseVisible ? (
        <form
          className="form-grid"
          onSubmit={(event) => {
            event.preventDefault()
            void submit()
          }}
        >
          <label>
            <span>Passphrase</span>
            <input
              data-testid="unlockField"
              type="password"
              autoComplete={setting ? 'new-password' : 'current-password'}
              autoFocus
              value={passphrase}
              onChange={(event) => setPassphrase(event.target.value)}
            />
          </label>
          {setting ? (
            <label>
              <span>Again</span>
              <input
                data-testid="unlockConfirmField"
                type="password"
                autoComplete="new-password"
                value={again}
                onChange={(event) => setAgain(event.target.value)}
              />
            </label>
          ) : null}
          {setting ? (
            <p className={`section__hint${tooShort ? ' section__hint--warning' : ''}`}>
              At least {MIN_PASSPHRASE_CHARS} characters.
            </p>
          ) : null}
          {mismatch ? <Notice kind="danger">The passphrases do not match.</Notice> : null}
          <div className="choice-row form-grid__actions">
            <button
              className="button button--primary"
              data-testid={setting ? 'setPassphraseButton' : 'unlockButton'}
              disabled={busy || !valid}
              type="submit"
            >
              {busy ? 'Working…' : setting ? 'Set passphrase' : 'Unlock'}
            </button>
          </div>
        </form>
      ) : null}
    </section>
  )
}
