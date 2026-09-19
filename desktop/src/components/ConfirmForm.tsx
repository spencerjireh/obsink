import { useState } from 'react'
import type { FormEvent } from 'react'

type Props = {
  title: string
  description: string
  // When set, the destructive button stays disabled until this exact value
  // is typed (compared case-insensitively for emails, exactly otherwise).
  expected?: string
  caseInsensitive?: boolean
  confirmLabel: string
  busy: boolean
  // Resolves true when the action succeeded; the form then closes. On
  // failure it stays open under the message notice.
  onConfirm: () => Promise<boolean>
  onCancel: () => void
}

// The confirmation pattern from DESIGN.md: inline, typed value, never a
// native dialog.
export function ConfirmForm({
  title,
  description,
  expected,
  caseInsensitive = false,
  confirmLabel,
  busy,
  onConfirm,
  onCancel,
}: Props) {
  const [typed, setTyped] = useState('')
  const matches =
    expected === undefined ||
    (caseInsensitive
      ? typed.trim().toLowerCase() === expected.toLowerCase()
      : typed.trim() === expected)

  async function handleSubmit(event: FormEvent) {
    event.preventDefault()
    if (!matches || busy) return
    if (await onConfirm()) {
      onCancel()
    }
  }

  return (
    <form className="confirm" onSubmit={(event) => void handleSubmit(event)}>
      <h3>{title}</h3>
      <p>{description}</p>
      {expected !== undefined ? (
        <label className="confirm__field">
          <span>
            Type <code>{expected}</code> to confirm.
          </span>
          <input
            className="mono"
            autoFocus
            autoCapitalize="off"
            autoCorrect="off"
            spellCheck={false}
            value={typed}
            onChange={(event) => setTyped(event.target.value)}
          />
        </label>
      ) : null}
      <div className="choice-row">
        <button className="button button--danger" disabled={!matches || busy} type="submit">
          {confirmLabel}
        </button>
        <button className="button button--ghost" disabled={busy} onClick={onCancel} type="button">
          Cancel
        </button>
      </div>
    </form>
  )
}
