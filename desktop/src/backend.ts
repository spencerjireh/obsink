import { invoke } from '@tauri-apps/api/core'
import { listen } from '@tauri-apps/api/event'
import { toCommandError, type Backend, type BackendEvent, type BackendEvents } from '@obsink/ui'

// The desktop Backend: every call is a Tauri command in src-tauri/src/main.rs
// (JS camelCase args become Rust snake_case), every event a Tauri event.
async function call<T>(command: string, args?: Record<string, unknown>): Promise<T> {
  try {
    return await invoke<T>(command, args)
  } catch (error) {
    throw toCommandError(error)
  }
}

export const tauriBackend: Backend = {
  platform: {
    kind: 'desktop',
    deviceNoun: 'this Mac',
    keyStoreNoun: 'the keychain',
    canOpenFolder: true,
    folderPlaceholder: '/Users/you/Documents/Notes',
    folderPrompt: {
      create: 'Where the vault lives on this Mac',
      connect: 'Where to put the vault on this Mac',
    },
  },
  getServerUrl: () => call('get_server_url'),
  getAuthCapabilities: () => call('get_auth_capabilities'),
  authEmailStart: (email) => call('auth_email_start', { email }),
  authEmailVerify: (email, code, inviteCode) =>
    call('auth_email_verify', { email, code, inviteCode }),
  getAccount: () => call('get_account'),
  createInvite: () => call('create_invite'),
  listInvites: () => call('list_invites'),
  revokeSession: (sessionId) => call('revoke_session', { sessionId }),
  signOut: () => call('sign_out'),
  deleteAccount: () => call('delete_account'),
  listRemoteVaults: () => call('list_remote_vaults'),
  addVault: (request) => call('add_vault', { request }),
  removeVault: (vaultId) => call('remove_vault', { vaultId }),
  deleteRemoteVault: (vaultId) => call('delete_remote_vault', { vaultId }),
  getVaultStates: () => call('get_vault_states'),
  syncVault: (vaultId) => call('sync_vault', { vaultId }),
  resolveConflict: (vaultId, resolutions) => call('resolve_conflict', { vaultId, resolutions }),
  getConflictPreview: (vaultId, path) => call('get_conflict_preview', { vaultId, path }),
  listActivity: (vaultId, limit) => call('list_activity', { vaultId, limit }),
  openVaultFolder: (vaultId) => call('open_vault_folder', { vaultId }),
  // Ask Rust to raise the settings window at a tab (the popover hides).
  openSettings: (target = {}) =>
    call('open_settings', {
      target: { tab: 'vaults', vault_id: null, add_vault: false, ...target },
    }),
  on<E extends BackendEvent>(event: E, handler: (payload: BackendEvents[E]) => void) {
    const registration = listen<BackendEvents[E]>(event, (tauriEvent) =>
      handler(tauriEvent.payload),
    )
    return () => {
      void registration.then((dispose) => dispose())
    }
  },
}
