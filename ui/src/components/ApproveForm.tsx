import { useEffect, useRef, useState } from 'react'
import type { FormEvent } from 'react'

// Spec §12.3: the fingerprint is 8 symbols of the invite alphabet.
const FINGERPRINT_LENGTH = 8

// What the backend compares: whitespace and dashes dropped, uppercased
// (the backend normalises again before it looks at the key).
function normalizeFingerprint(value: string): string {
  return value.replace(/[\s-]/g, '').toUpperCase()
}

type Props = {
  // The pending device, as its row names it.
  deviceName: string
  busy: boolean
  // Resolves true when the key was wrapped and posted; the form then
  // closes. On a mismatch it stays open under the message notice.
  onConfirm: (fingerprint: string) => Promise<boolean>
  onCancel: () => void
}

// Spec §15.3: the approver types the 8 characters the other device shows.
// Same shape as ConfirmForm: inline, never a native dialog.
export function ApproveForm({ deviceName, busy, onConfirm, onCancel }: Props) {
  const [typed, setTyped] = useState('')
  const formRef = useRef<HTMLFormElement>(null)

  // The form opens below the row that asked for it, often past the fold.
  useEffect(() => {
    // jsdom (the ui unit tests) has no scrollIntoView.
    formRef.current?.scrollIntoView?.({ block: 'nearest' })
  }, [])
  const normalized = normalizeFingerprint(typed)
  const complete = normalized.length === FINGERPRINT_LENGTH

  async function handleSubmit(event: FormEvent) {
    event.preventDefault()
    if (!complete || busy) return
    if (await onConfirm(normalized)) {
      onCancel()
    }
  }

  return (
    <form ref={formRef} className="confirm" onSubmit={(event) => void handleSubmit(event)}>
      <h3>Approve {deviceName}</h3>
      <p>
        Type the {FINGERPRINT_LENGTH} characters shown on {deviceName}.
      </p>
      <label className="confirm__field">
        <span>Fingerprint</span>
        <input
          data-testid="approveFingerprintField"
          className="mono"
          autoFocus
          autoCapitalize="characters"
          autoCorrect="off"
          spellCheck={false}
          maxLength={FINGERPRINT_LENGTH + 1}
          value={typed}
          onChange={(event) => setTyped(event.target.value.toUpperCase())}
        />
      </label>
      <div className="choice-row">
        <button
          className="button button--primary"
          data-testid="approveSubmitButton"
          disabled={!complete || busy}
          type="submit"
        >
          Approve
        </button>
        <button
          className="button button--ghost"
          data-testid="approveCancelButton"
          disabled={busy}
          onClick={onCancel}
          type="button"
        >
          Cancel
        </button>
      </div>
    </form>
  )
}
