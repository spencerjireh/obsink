import type { SettingsTab } from '../types'

const TABS: { id: SettingsTab; label: string }[] = [
  { id: 'vaults', label: 'Vaults' },
  { id: 'account', label: 'Account' },
  { id: 'activity', label: 'Activity' },
]

export function Tabs({
  tab,
  onChange,
}: {
  tab: SettingsTab
  onChange: (tab: SettingsTab) => void
}) {
  return (
    <nav className="tabs" role="tablist" aria-label="Settings">
      {TABS.map((entry) => (
        <button
          key={entry.id}
          className="tab"
          role="tab"
          data-testid="settingsTab"
          data-tab={entry.id}
          aria-selected={tab === entry.id}
          onClick={() => onChange(entry.id)}
          type="button"
        >
          {entry.label}
        </button>
      ))}
    </nav>
  )
}
