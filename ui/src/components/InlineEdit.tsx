import { useState } from 'react'
import type { FormEvent, ReactNode } from 'react'

type Props = {
  // What the field holds when editing starts.
  value: string
  // The control's label while closed (`Rename`, `Move folder`); it reads
  // `Save` while the field is open, so one test id covers both.
  label: string
  fieldTestId: string
  buttonTestId: string
  busy: boolean
  // Shown while closed, in place of the field.
  children: ReactNode
  // Resolves true when the change landed; the field then closes.
  onSave: (next: string) => Promise<boolean>
  // Field attributes for a path rather than a name.
  mono?: boolean
  placeholder?: string
}

// A value edited in place (DESIGN.md §5 `Rename`, `Move folder`): the text,
// one button that opens the field and then saves it, and `Cancel`.
export function InlineEdit({
  value,
  label,
  fieldTestId,
  buttonTestId,
  busy,
  children,
  onSave,
  mono = false,
  placeholder,
}: Props) {
  const [draft, setDraft] = useState<string | null>(null)
  const editing = draft !== null
  const valid = editing && draft.trim().length > 0 && draft.trim() !== value

  async function submit(event?: FormEvent) {
    event?.preventDefault()
    if (!editing || busy || !valid) return
    if (await onSave(draft.trim())) setDraft(null)
  }

  const button = (
    <button
      className="button button--ghost button--small"
      data-testid={buttonTestId}
      disabled={busy || (editing && !valid)}
      onClick={editing ? undefined : () => setDraft(value)}
      type={editing ? 'submit' : 'button'}
    >
      {editing ? 'Save' : label}
    </button>
  )

  if (!editing) {
    return (
      <span className="inline-edit">
        {children}
        {button}
      </span>
    )
  }

  return (
    <form className="inline-edit" onSubmit={(event) => void submit(event)}>
      <input
        className={mono ? 'mono' : undefined}
        data-testid={fieldTestId}
        autoFocus
        placeholder={placeholder}
        value={draft}
        onChange={(event) => setDraft(event.target.value)}
        onKeyDown={(event) => {
          if (event.key === 'Escape') setDraft(null)
        }}
      />
      {button}
      <button
        className="button button--ghost button--small"
        disabled={busy}
        onClick={() => setDraft(null)}
        type="button"
      >
        Cancel
      </button>
    </form>
  )
}
