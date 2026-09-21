import { useEffect, useState } from 'react'
import type { SettingsTab } from '../types'
import { useBackend } from '../backend'
import { useAccount } from '../hooks/useAccount'
import { useVaultStates } from '../hooks/useVaultStates'
import { Notice } from '../components/Notices'
import { AccountTab } from './account/AccountTab'
import { ActivityTab } from './activity/ActivityTab'
import { Tabs } from './Tabs'
import { VaultsTab } from './vaults/VaultsTab'

// The settings window: everything that is not a glance at status.
export function SettingsApp() {
  const backend = useBackend()
  const [tab, setTab] = useState<SettingsTab>('vaults')
  const [selectedId, setSelectedId] = useState<string | null>(null)
  const [adding, setAdding] = useState(false)
  const [message, setMessage] = useState('')
  const account = useAccount(setMessage)
  const { states, loaded, refresh } = useVaultStates(account.fail)

  // Nothing selected yet, or the selection disappeared: show the first vault.
  useEffect(() => {
    if (!loaded) return
    if (states.length === 0) {
      setSelectedId(null)
      return
    }
    if (!states.some((s) => s.id === selectedId)) {
      setSelectedId(states[0].id)
    }
  }, [loaded, states, selectedId])

  // The popover (or the tray) asks for a tab, a vault, or the add flow.
  useEffect(
    () =>
      backend.on('settings://navigate', (target) => {
        setTab(target.tab)
        if (target.vault_id) setSelectedId(target.vault_id)
        setAdding(target.add_vault)
        setMessage('')
        void refresh()
      }),
    [backend, refresh],
  )

  function changeTab(next: SettingsTab) {
    setTab(next)
    setMessage('')
  }

  return (
    <div className="settings">
      <Tabs tab={tab} onChange={changeTab} />
      <div className="settings__pane">
        {tab === 'vaults' ? (
          <VaultsTab
            states={states}
            loaded={loaded}
            selectedId={selectedId}
            adding={adding}
            account={account}
            message={message}
            notify={setMessage}
            onError={account.fail}
            onSelect={(vaultId) => {
              setSelectedId(vaultId || null)
              setAdding(false)
              setMessage('')
            }}
            onAdded={(vaultId) => setSelectedId(vaultId)}
            onAdd={() => {
              setAdding(true)
              setMessage('')
            }}
            onCloseAdd={() => {
              setAdding(false)
              setMessage('')
            }}
            onSignIn={() => changeTab('account')}
          />
        ) : null}
        {tab === 'account' ? (
          <main className="main">
            {message ? <Notice>{message}</Notice> : null}
            <AccountTab account={account} busy={account.busy} />
          </main>
        ) : null}
        {tab === 'activity' ? (
          <main className="main">
            <ActivityTab states={states} onError={account.fail} />
          </main>
        ) : null}
      </div>
    </div>
  )
}
