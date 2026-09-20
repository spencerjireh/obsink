import type { VaultStateInfo } from '../types'
import { stateText, stateTone } from '../lib/vault-state'
import { StateDot } from '../components/StateDot'

type Props = {
  info: VaultStateInfo
  syncing: boolean
  onOpen: () => void
  onOpenFolder: () => void
}

export function VaultRow({ info, syncing, onOpen, onOpenFolder }: Props) {
  return (
    <li className="vault-row">
      <button className="vault-row__main" onClick={onOpen} type="button">
        <StateDot tone={syncing ? 'pending' : stateTone(info)} />
        <span className="vault-row__name">{info.name}</span>
        <span className="vault-row__state">{syncing ? 'Syncing…' : stateText(info)}</span>
      </button>
      <button
        className="icon-button"
        aria-label={`Open folder for ${info.name}`}
        title="Open folder"
        onClick={onOpenFolder}
        type="button"
      >
        <svg viewBox="0 0 16 16" width="14" height="14" aria-hidden="true" focusable="false">
          <path
            d="M1.5 3.5A1.5 1.5 0 0 1 3 2h3.2l1.6 1.5H13a1.5 1.5 0 0 1 1.5 1.5v7A1.5 1.5 0 0 1 13 13.5H3A1.5 1.5 0 0 1 1.5 12z"
            fill="none"
            stroke="currentColor"
            strokeWidth="1.2"
          />
        </svg>
      </button>
    </li>
  )
}
