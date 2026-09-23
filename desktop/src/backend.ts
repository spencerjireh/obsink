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
      download: 'Where to put the vault on this Mac',
    },
  },
  getServerUrl: () => call('get_server_url'),
  getProtocol: () => call('get_protocol'),
  getAuthCapabilities: () => call('get_auth_capabilities'),
  authEmailStart: (email) => call('auth_email_start', { email }),
  authEmailVerify: (email, code, inviteCode) =>
    call('auth_email_verify', { email, code, inviteCode }),
  getAccount: () => call('get_account'),
  setPassphrase: (passphrase) => call('set_passphrase', { passphrase }),
  unlock: (passphrase) => call('unlock', { passphrase }),
  changePassphrase: (current, next) => call('change_passphrase', { current, next }),
  createInvite: () => call('create_invite'),
  listInvites: () => call('list_invites'),
  revokeDevice: (deviceId) => call('revoke_device', { deviceId }),
  renameDevice: (deviceId, name) => call('rename_device', { deviceId, name }),
  signOut: () => call('sign_out'),
  deleteAccount: () => call('delete_account'),
  listVaults: () => call('list_vaults'),
  createVault: (request) => call('create_vault', { request }),
  downloadVault: (request) => call('download_vault', { request }),
  renameVault: (vaultId, name) => call('rename_vault', { vaultId, name }),
  moveVaultFolder: (vaultId, localPath) => call('move_vault_folder', { vaultId, localPath }),
  removeVault: (vaultId) => call('remove_vault', { vaultId }),
  deleteRemoteVault: (vaultId) => call('delete_remote_vault', { vaultId }),
  syncVault: (vaultId) => call('sync_vault', { vaultId }),
  resolveConflict: (vaultId, resolutions) => call('resolve_conflict', { vaultId, resolutions }),
  getConflictPreview: (vaultId, path) => call('get_conflict_preview', { vaultId, path }),
  listActivity: (vaultId, limit) => call('list_activity', { vaultId, limit }),
  listFiles: (vaultId) => call('list_files', { vaultId }),
  listVersions: (vaultId, path) => call('list_versions', { vaultId, path }),
  previewVersion: (vaultId, path, name) => call('preview_version', { vaultId, path, name }),
  restoreVersion: (vaultId, path, name) => call('restore_version', { vaultId, path, name }),
  listTrash: (vaultId) => call('list_trash', { vaultId }),
  previewTrash: (vaultId, path) => call('preview_trash', { vaultId, path }),
  restoreTrash: (vaultId, path) => call('restore_trash', { vaultId, path }),
  openVaultFolder: (vaultId) => call('open_vault_folder', { vaultId }),
  // Ask Rust to raise the settings window at a tab (the popover hides).
  openSettings: (target = {}) =>
    call('open_settings', {
      target: { tab: 'vaults', vault_id: null, add_vault: false, download: false, ...target },
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
