import { useEffect } from 'react'
import type { ComponentProps } from 'react'
import type { SetupFocus } from '../types'
import { AccountSection } from './AccountSection'
import { AddVaultForm } from './AddVaultForm'
import { Notice } from './Notices'

type Props = {
  focus: SetupFocus | null
  message: string
  canClose: boolean
  onClose: () => void
  account: ComponentProps<typeof AccountSection>
  addVault: ComponentProps<typeof AddVaultForm>
}

export function SetupView({ focus, message, canClose, onClose, account, addVault }: Props) {
  // "Add vault" and "Account" in the sidebar both open this view; scroll to
  // the section the user asked for. Opening on first launch has no focus.
  useEffect(() => {
    if (focus) {
      document.getElementById(`setup-${focus.section}`)?.scrollIntoView({ block: 'start' })
    }
  }, [focus])

  return (
    <main className="main">
      <header className="pane-header">
        <div className="pane-header__title">
          <h1>Setup</h1>
          <p className="pane-header__meta">
            Sign in to a server, then create a vault or connect to one.
          </p>
        </div>
        {canClose ? (
          <button className="button button--ghost" onClick={onClose} type="button">
            Done
          </button>
        ) : null}
      </header>
      {message ? <Notice>{message}</Notice> : null}
      <AccountSection {...account} />
      <AddVaultForm {...addVault} />
    </main>
  )
}
