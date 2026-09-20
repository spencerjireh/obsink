import type { SettingsTarget } from '../types'
import { call } from './tauri'

// Ask Rust to raise the settings window at a tab (the popover hides).
export function openSettings(target: Partial<SettingsTarget> = {}): Promise<void> {
  return call('open_settings', {
    target: { tab: 'vaults', vault_id: null, add_vault: false, ...target },
  })
}

export function openFolder(vaultId: string): Promise<void> {
  return call('open_vault_folder', { vaultId })
}
