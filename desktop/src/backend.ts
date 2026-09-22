import { invoke } from '@tauri-apps/api/core'
import { listen } from '@tauri-apps/api/event'
import {
  toCommandError,
  type AccountState,
  type Backend,
  type BackendEvent,
  type BackendEvents,
  type VaultStateInfo,
} from '@obsink/ui'

// The desktop Backend: every call is a Tauri command in src-tauri/src/main.rs
// (JS camelCase args become Rust snake_case), every event a Tauri event.
async function call<T>(command: string, args?: Record<string, unknown>): Promise<T> {
  try {
    return await invoke<T>(command, args)
  } catch (error) {
    throw toCommandError(error)
  }
}

// The v2 command set still behind these methods (OBS-140 replaces them):
// the Rust side knows local vaults only and no account key.
const notYet = (what: string) =>
  Promise.reject(
    toCommandError({ kind: 'other', message: `${what} arrives with the next desktop build.` }),
  )

type LegacyVaultState = VaultStateInfo['state'] | { kind: 'foreign' } | { kind: 'no_key' }
type LegacyVaultStateInfo = Omit<VaultStateInfo, 'state' | 'revision' | 'bytes' | 'devices'> & {
  state: LegacyVaultState
  server_url: string
}

// The current Rust `get_vault_states` shape, mapped onto the v3 list until
// OBS-140 lands the merged list.
function fromLegacy(info: LegacyVaultStateInfo): VaultStateInfo {
  const state: VaultStateInfo['state'] =
    info.state.kind === 'foreign' || info.state.kind === 'no_key' ? { kind: 'locked' } : info.state
  return {
    id: info.id,
    name: info.name,
    local_path: info.local_path,
    state,
    last_synced: info.last_synced,
    revision: 0,
    bytes: 0,
    devices: [],
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
  // Until OBS-140 the desktop trusts the server it is built for.
  getProtocol: () => Promise.resolve({ server: null, client: 3 }),
  getAuthCapabilities: () => call('get_auth_capabilities'),
  authEmailStart: (email) => call('auth_email_start', { email }),
  authEmailVerify: (email, code, inviteCode) =>
    call('auth_email_verify', { email, code, inviteCode }),
  getAccount: () => call<AccountState>('get_account'),
  setPassphrase: () => notYet('Setting the passphrase'),
  unlock: () => notYet('Unlocking'),
  changePassphrase: () => notYet('Changing the passphrase'),
  createInvite: () => call('create_invite'),
  listInvites: () => call('list_invites'),
  revokeDevice: (deviceId) => call('revoke_session', { sessionId: deviceId }),
  renameDevice: () => notYet('Renaming a device'),
  signOut: () => call('sign_out'),
  deleteAccount: () => call('delete_account'),
  listVaults: async () => (await call<LegacyVaultStateInfo[]>('get_vault_states')).map(fromLegacy),
  createVault: () => notYet('Creating a vault'),
  downloadVault: () => notYet('Downloading a vault'),
  renameVault: () => notYet('Renaming a vault'),
  moveVaultFolder: () => notYet('Moving the folder'),
  removeVault: (vaultId) => call('remove_vault', { vaultId }),
  deleteRemoteVault: (vaultId) => call('delete_remote_vault', { vaultId }),
  syncVault: (vaultId) => call('sync_vault', { vaultId }),
  resolveConflict: (vaultId, resolutions) => call('resolve_conflict', { vaultId, resolutions }),
  getConflictPreview: (vaultId, path) => call('get_conflict_preview', { vaultId, path }),
  listActivity: (vaultId, limit) => call('list_activity', { vaultId, limit }),
  listFiles: () => notYet('File history'),
  listVersions: () => notYet('File history'),
  previewVersion: () => notYet('File history'),
  restoreVersion: () => notYet('Restoring a version'),
  listTrash: () => notYet('Recently deleted'),
  previewTrash: () => notYet('Recently deleted'),
  restoreTrash: () => notYet('Restoring a deleted file'),
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
