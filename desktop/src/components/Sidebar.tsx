import type { LocalVault, View } from '../types'

type Props = {
  vaults: LocalVault[]
  busy: boolean
  view: View
  onSelectVault: (vault: LocalVault) => void
  onAddVault: () => void
  onAccount: () => void
}

export function Sidebar({ vaults, busy, view, onSelectVault, onAddVault, onAccount }: Props) {
  return (
    <aside className="sidebar" aria-label="Vaults">
      <div className="sidebar__brand">
        <svg className="sidebar__mark" viewBox="0 0 1024 1024" aria-hidden="true" focusable="false">
          <g transform="translate(512 512) scale(1.6) translate(-512 -512)">
            <g fill="none" stroke="currentColor" strokeWidth="96">
              <path d="M320.5 351.3A250 250 0 0 1 672.7 320.5" />
              <path d="M703.5 672.7A250 250 0 0 1 351.3 703.5" />
            </g>
            <g fill="currentColor">
              <polygon points="496,162 496,362 656,262" transform="rotate(40 512 512)" />
              <polygon points="496,162 496,362 656,262" transform="rotate(220 512 512)" />
            </g>
            <circle cx="512" cy="512" r="150" fill="currentColor" />
          </g>
        </svg>
        <span className="sidebar__name">ObSink</span>
      </div>
      <h2 className="sidebar__heading">Vaults</h2>
      <nav className="sidebar__vaults">
        {vaults.length === 0 ? <p className="empty-state">No vaults yet.</p> : null}
        {vaults.map((vault) => {
          const current = vault.active && view === 'vault'
          return (
            <button
              key={vault.id}
              className={`vault-item${vault.active ? ' vault-item--active' : ''}`}
              aria-current={current ? 'page' : undefined}
              disabled={busy}
              onClick={() => onSelectVault(vault)}
              title={vault.local_path}
              type="button"
            >
              <span className="vault-item__name">{vault.name}</span>
              <span className="vault-item__path">{vault.local_path}</span>
            </button>
          )
        })}
      </nav>
      <div className="sidebar__footer">
        <button className="button button--ghost" disabled={busy} onClick={onAddVault} type="button">
          Add vault
        </button>
        <button className="button button--ghost" disabled={busy} onClick={onAccount} type="button">
          Account
        </button>
      </div>
    </aside>
  )
}
