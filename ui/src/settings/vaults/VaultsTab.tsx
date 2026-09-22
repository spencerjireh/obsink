import type { VaultStateInfo } from '../../types'
import type { Account } from '../../hooks/useAccount'
import { onThisDevice, stateText, stateTone } from '../../lib/vault-state'
import { EmptyState } from '../../components/EmptyState'
import { Notice } from '../../components/Notices'
import { StateDot } from '../../components/StateDot'
import { VaultFlow } from './VaultFlow'
import { VaultPage } from './VaultPage'

// What the pane on the right shows besides a vault page.
export type VaultFlowTarget =
  { kind: 'create' } | { kind: 'download'; vault_id: string; vault_name: string }

type Props = {
  states: VaultStateInfo[]
  loaded: boolean
  selectedId: string | null
  flow: VaultFlowTarget | null
  account: Account
  message: string
  notify: (message: string) => void
  onError: (error: unknown) => void
  onSelect: (vaultId: string) => void
  // A vault landed on this device; the flow stays open on its done card.
  onAdded: (vaultId: string) => void
  onFlow: (flow: VaultFlowTarget | null) => void
  onSignIn: () => void
}

// Every vault of the account on the left (spec §15.1), the selected vault
// or a flow on the right.
export function VaultsTab({
  states,
  loaded,
  selectedId,
  flow,
  account,
  message,
  notify,
  onError,
  onSelect,
  onAdded,
  onFlow,
  onSignIn,
}: Props) {
  const selected = states.find((s) => s.id === selectedId) ?? null

  return (
    <div className="vaults-layout">
      <aside className="vault-list" aria-label="Vaults">
        <nav className="vault-list__items">
          {loaded && states.length === 0 ? <EmptyState>No vaults yet.</EmptyState> : null}
          {states.map((info) => {
            const current = !flow && info.id === selectedId
            const here = onThisDevice(info)
            return (
              <div
                key={info.id}
                className={`vault-item${current ? ' vault-item--active' : ''}${
                  here ? '' : ' vault-item--elsewhere'
                }`}
              >
                <button
                  className="vault-item__button"
                  aria-current={current ? 'page' : undefined}
                  data-testid="vaultCard"
                  data-vault-id={info.id}
                  onClick={() => onSelect(info.id)}
                  type="button"
                >
                  <span className="vault-item__name">
                    <StateDot tone={stateTone(info)} /> {info.name}
                  </span>
                  <span className="vault-item__path" data-testid="vaultStateText">
                    {stateText(info)}
                  </span>
                </button>
                {info.state.kind === 'not_on_device' ? (
                  <button
                    className="button button--ghost button--small"
                    data-testid="downloadVaultButton"
                    data-vault-id={info.id}
                    disabled={account.busy}
                    onClick={() =>
                      onFlow({ kind: 'download', vault_id: info.id, vault_name: info.name })
                    }
                    type="button"
                  >
                    Download
                  </button>
                ) : null}
              </div>
            )
          })}
        </nav>
        <div className="vault-list__footer">
          <button
            className="button button--ghost"
            data-testid="createVaultButton"
            disabled={flow?.kind === 'create'}
            onClick={() => onFlow({ kind: 'create' })}
            type="button"
          >
            Create vault
          </button>
        </div>
      </aside>
      <main className="main">
        {flow ? (
          <VaultFlow
            key={flow.kind === 'download' ? flow.vault_id : 'create'}
            flow={flow}
            message={message}
            notify={notify}
            onError={onError}
            onAdded={(vault) => onAdded(vault.id)}
            onClose={() => onFlow(null)}
          />
        ) : selected ? (
          <VaultPage
            key={selected.id}
            info={selected}
            account={account}
            message={message}
            notify={notify}
            onError={onError}
            onSignIn={onSignIn}
            onDownload={() =>
              onFlow({ kind: 'download', vault_id: selected.id, vault_name: selected.name })
            }
            onGone={() => onSelect('')}
          />
        ) : (
          <div className="vault-page">
            <header className="pane-header">
              <div className="pane-header__title">
                <h1>{loaded && states.length === 0 ? 'No vaults yet' : 'No vault selected'}</h1>
                <p className="pane-header__meta">
                  {loaded && states.length === 0
                    ? 'Create a vault to start syncing.'
                    : 'Pick a vault on the left.'}
                </p>
              </div>
              <button
                className="button button--primary"
                data-testid="createVaultButton"
                onClick={() => onFlow({ kind: 'create' })}
                type="button"
              >
                Create vault
              </button>
            </header>
            {message ? <Notice>{message}</Notice> : null}
          </div>
        )}
      </main>
    </div>
  )
}
