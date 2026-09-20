import type { StateTone } from '../lib/vault-state'

// A coloured dot next to a vault name; the state text says what it means.
export function StateDot({ tone }: { tone: StateTone }) {
  return <span className={`state-dot state-dot--${tone}`} aria-hidden="true" />
}
