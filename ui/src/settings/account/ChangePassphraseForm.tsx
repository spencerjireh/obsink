import { useState } from 'react'
import type { Account } from '../../hooks/useAccount'
import { Notice } from '../../components/Notices'
import { MIN_PASSPHRASE_CHARS } from './UnlockForm'

// DESIGN.md §5 `Change passphrase`: current, new twice. The account key
// stays the same; only its wrapping changes (spec §6.1).
export function ChangePassphraseForm({ account, busy }: { account: Account; busy: boolean }) {
  const [current, setCurrent] = useState('')
  const [next, setNext] = useState('')
  const [again, setAgain] = useState('')
  const [mismatch, setMismatch] = useState(false)
  const tooShort = next.length > 0 && next.length < MIN_PASSPHRASE_CHARS
  const valid = current.length > 0 && next.length >= MIN_PASSPHRASE_CHARS && again.length > 0

  async function submit() {
    if (!valid || busy) return
    if (next !== again) {
      setMismatch(true)
      return
    }
    setMismatch(false)
    if (await account.changePassphrase(current, next)) {
      setCurrent('')
      setNext('')
      setAgain('')
    }
  }

  return (
    <form
      className="form-grid"
      onSubmit={(event) => {
        event.preventDefault()
        void submit()
      }}
    >
      <label>
        <span>Current passphrase</span>
        <input
          data-testid="currentPassphraseField"
          type="password"
          autoComplete="current-password"
          value={current}
          onChange={(event) => setCurrent(event.target.value)}
        />
      </label>
      <label>
        <span>New passphrase</span>
        <input
          data-testid="newPassphraseField"
          type="password"
          autoComplete="new-password"
          value={next}
          onChange={(event) => setNext(event.target.value)}
        />
      </label>
      <label>
        <span>Again</span>
        <input
          data-testid="newPassphraseConfirmField"
          type="password"
          autoComplete="new-password"
          value={again}
          onChange={(event) => setAgain(event.target.value)}
        />
      </label>
      <p className={`section__hint${tooShort ? ' section__hint--warning' : ''}`}>
        At least {MIN_PASSPHRASE_CHARS} characters.
      </p>
      {mismatch ? <Notice kind="danger">The passphrases do not match.</Notice> : null}
      <div className="choice-row form-grid__actions">
        <button
          className="button button--primary"
          data-testid="changePassphraseButton"
          disabled={busy || !valid}
          type="submit"
        >
          {busy ? 'Working…' : 'Change passphrase'}
        </button>
      </div>
    </form>
  )
}
