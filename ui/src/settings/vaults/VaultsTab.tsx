import type { VaultStateInfo } from '../../types'
import type { Account } from '../../hooks/useAccount'
import { stateText, stateTone } from '../../lib/vault-state'
import { EmptyState } from '../../components/EmptyState'
import { Notice } from '../../components/Notices'
import { StateDot } from '../../components/StateDot'
import { AddVaultFlow } from './AddVaultFlow'
import { VaultPage } from './VaultPage'

type Props = {
  states: VaultStateInfo[]
  loaded: boolean
  selectedId: string | null
  adding: boolean
  account: Account
  message: string
  notify: (message: string) => void
  onError: (error: unknown) => void
  onSelect: (vaultId: string) => void
  // A vault was configured; the add flow stays open on its done card.
  onAdded: (vaultId: string) => void
  onAdd: () => void
  onCloseAdd: () => void
  onSignIn: () => void
}

// Vault list on the left, the selected vault (or the add flow) on the right.
export function VaultsTab({
  states,
  loaded,
  selectedId,
  adding,
  account,
  message,
  notify,
  onError,
  onSelect,
  onAdded,
  onAdd,
  onCloseAdd,
  onSignIn,
}: Props) {
  const selected = states.find((s) => s.id === selectedId) ?? null

  return (
    <div className="vaults-layout">
      <aside className="vault-list" aria-label="Vaults">
        <nav className="vault-list__items">
          {loaded && states.length === 0 ? <EmptyState>No vaults yet.</EmptyState> : null}
          {states.map((info) => {
            const current = !adding && info.id === selectedId
            return (
              <button
                key={info.id}
                className={`vault-item${current ? ' vault-item--active' : ''}`}
                aria-current={current ? 'page' : undefined}
                data-testid="vaultCard"
                data-vault-id={info.id}
                onClick={() => onSelect(info.id)}
                type="button"
              >
                <span className="vault-item__name">
                  <StateDot tone={stateTone(info)} /> {info.name}
                </span>
                <span className="vault-item__path">{stateText(info)}</span>
              </button>
            )
          })}
        </nav>
        <div className="vault-list__footer">
          <button
            className="button button--ghost"
            data-testid="addVaultButton"
            disabled={adding}
            onClick={onAdd}
            type="button"
          >
            Add vault
          </button>
        </div>
      </aside>
      <main className="main">
        {adding ? (
          <AddVaultFlow
            account={account}
            message={message}
            notify={notify}
            onError={onError}
            onAdded={(vault) => onAdded(vault.id)}
            onClose={onCloseAdd}
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
            onGone={() => onSelect('')}
          />
        ) : (
          <div className="vault-page">
            <header className="pane-header">
              <div className="pane-header__title">
                <h1>{loaded && states.length === 0 ? 'No vaults yet' : 'No vault selected'}</h1>
                <p className="pane-header__meta">
                  {loaded && states.length === 0
                    ? 'Add a vault to start syncing.'
                    : 'Pick a vault on the left.'}
                </p>
              </div>
              <button
                className="button button--primary"
                data-testid="addVaultButton"
                onClick={onAdd}
                type="button"
              >
                Add vault
              </button>
            </header>
            {message ? <Notice>{message}</Notice> : null}
          </div>
        )}
      </main>
    </div>
  )
}
