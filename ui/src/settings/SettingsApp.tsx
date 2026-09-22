import { useEffect, useState } from 'react'
import type { ProtocolInfo, SettingsTab } from '../types'
import { useBackend } from '../backend'
import { useAccount } from '../hooks/useAccount'
import { useVaultStates } from '../hooks/useVaultStates'
import { Notice } from '../components/Notices'
import { AccountTab } from './account/AccountTab'
import { SignInForm } from './account/SignInForm'
import { UnlockForm } from './account/UnlockForm'
import { ActivityTab } from './activity/ActivityTab'
import { Tabs } from './Tabs'
import { UpdateRequired } from './UpdateRequired'
import { VaultsTab, type VaultFlowTarget } from './vaults/VaultsTab'

// The downloads live on the website; the protocol page points there.
const SITE_URL = 'https://obsink.spencerjireh.com'

// The settings window: everything that is not a glance at status.
export function SettingsApp() {
  const backend = useBackend()
  const [tab, setTab] = useState<SettingsTab>('vaults')
  const [selectedId, setSelectedId] = useState<string | null>(null)
  const [flow, setFlow] = useState<VaultFlowTarget | null>(null)
  const [message, setMessage] = useState('')
  const [protocol, setProtocol] = useState<ProtocolInfo | null>(null)
  const account = useAccount(setMessage)
  const { states, loaded, refresh } = useVaultStates(account.fail)

  // The protocol gate (spec §15.5), once per launch.
  useEffect(() => {
    void backend
      .getProtocol()
      .then(setProtocol)
      .catch(() => setProtocol(null))
  }, [backend])

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

  // The popover (or the tray) asks for a tab, a vault, a flow.
  useEffect(
    () =>
      backend.on('settings://navigate', (target) => {
        setTab(target.tab)
        if (target.vault_id) setSelectedId(target.vault_id)
        if (target.add_vault) {
          setFlow({ kind: 'create' })
        } else if (target.download && target.vault_id) {
          const vault = states.find((s) => s.id === target.vault_id)
          setFlow({ kind: 'download', vault_id: target.vault_id, vault_name: vault?.name ?? '' })
        } else {
          setFlow(null)
        }
        setMessage('')
        void refresh()
      }),
    [backend, refresh, states],
  )

  function changeTab(next: SettingsTab) {
    setTab(next)
    setMessage('')
  }

  if (protocol && protocol.server !== null && protocol.server !== protocol.client) {
    return (
      <div className="settings">
        <div className="settings__pane">
          <main className="main">
            <UpdateRequired protocol={protocol} siteUrl={SITE_URL} />
          </main>
        </div>
      </div>
    )
  }

  // Signed out or locked: the Vaults tab is the sign-in, then the unlock
  // (spec §12.1); nothing about vaults shows before the key is at hand.
  const gate =
    account.account?.kind === 'signed_out' ? (
      <section className="section" aria-labelledby="signin-heading">
        <div className="section__heading">
          <h2 id="signin-heading">Sign in</h2>
          <span className="section__hint mono">{account.serverUrl}</span>
        </div>
        {message ? <Notice>{message}</Notice> : null}
        <SignInForm account={account} busy={account.busy} />
      </section>
    ) : account.locked ? (
      <>
        {message ? <Notice>{message}</Notice> : null}
        <UnlockForm account={account} busy={account.busy} />
      </>
    ) : null

  return (
    <div className="settings">
      <Tabs tab={tab} onChange={changeTab} />
      <div className="settings__pane">
        {tab === 'vaults' ? (
          gate ? (
            <main className="main">{gate}</main>
          ) : (
            <VaultsTab
              states={states}
              loaded={loaded}
              selectedId={selectedId}
              flow={flow}
              account={account}
              message={message}
              notify={setMessage}
              onError={account.fail}
              onSelect={(vaultId) => {
                setSelectedId(vaultId || null)
                setFlow(null)
                setMessage('')
              }}
              onAdded={(vaultId) => setSelectedId(vaultId)}
              onFlow={(next) => {
                setFlow(next)
                setMessage('')
              }}
              onSignIn={() => changeTab('account')}
            />
          )
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
