import { createContext, useContext, type ReactNode } from 'react'
import type {
  AccountState,
  ActivityEvent,
  AddVaultMode,
  AuthCapabilities,
  ConflictPreview,
  InviteInfo,
  LocalVault,
  ProgressEnvelope,
  RemoteVault,
  ResolutionChoice,
  SettingsTarget,
  SyncResponse,
  VaultStateInfo,
} from './types'

// The one seam between the screens and a platform. The desktop app answers
// every call with a Tauri command and every event with a Tauri event; the
// browser client answers from a worker (wasm + fetch + the File System
// Access API). Field names in the payloads mirror the Rust structs.

export type AddVaultRequest = {
  mode: AddVaultMode
  // A folder path on desktop; the id of a picked directory handle on the web.
  local_path: string
  vault_name: string
  vault_id: string
  passphrase: string
}

export type Resolution = { path: string; choice: ResolutionChoice }

export type BackendEvents = {
  // Something about a vault (or the account) changed; re-read what you show.
  'state://changed': { vault_id: string | null }
  'sync://progress': ProgressEnvelope
  'popover://opened': void
  'tray://sync-now': void
  'settings://navigate': SettingsTarget
}
export type BackendEvent = keyof BackendEvents

// What the copy and the folder step need to know about where they run.
export type Platform = {
  kind: 'desktop' | 'web'
  // "this Mac" | "this browser"
  deviceNoun: string
  // "the keychain" | "this browser"
  keyStoreNoun: string
  // Whether "Open folder" can reveal the vault folder.
  canOpenFolder: boolean
  // Shown in the path field when the folder is typed rather than picked.
  folderPlaceholder: string
}

export interface Backend {
  readonly platform: Platform
  getServerUrl(): Promise<string>
  getAuthCapabilities(): Promise<AuthCapabilities>
  // Resolves to the code itself on a dev server that returns it inline.
  authEmailStart(email: string): Promise<string | null>
  authEmailVerify(email: string, code: string, inviteCode: string | null): Promise<AccountState>
  getAccount(): Promise<AccountState>
  createInvite(): Promise<InviteInfo>
  listInvites(): Promise<InviteInfo[]>
  revokeSession(sessionId: string): Promise<AccountState>
  signOut(): Promise<void>
  deleteAccount(): Promise<void>
  listRemoteVaults(): Promise<RemoteVault[]>
  addVault(request: AddVaultRequest): Promise<LocalVault>
  // Present where folders are picked rather than typed (the browser).
  pickFolder?(): Promise<{ id: string; name: string }>
  removeVault(vaultId: string): Promise<void>
  deleteRemoteVault(vaultId: string): Promise<void>
  getVaultStates(): Promise<VaultStateInfo[]>
  syncVault(vaultId: string): Promise<SyncResponse>
  resolveConflict(vaultId: string, resolutions: Resolution[]): Promise<SyncResponse>
  getConflictPreview(vaultId: string, path: string): Promise<ConflictPreview>
  listActivity(vaultId: string | null, limit: number): Promise<ActivityEvent[]>
  openVaultFolder(vaultId: string): Promise<void>
  openSettings(target?: Partial<SettingsTarget>): Promise<void>
  // Subscribe; the returned function unsubscribes.
  on<E extends BackendEvent>(event: E, handler: (payload: BackendEvents[E]) => void): () => void
}

const BackendContext = createContext<Backend | null>(null)

export function BackendProvider({ backend, children }: { backend: Backend; children: ReactNode }) {
  return <BackendContext.Provider value={backend}>{children}</BackendContext.Provider>
}

// eslint-disable-next-line react-refresh/only-export-components
export function useBackend(): Backend {
  const backend = useContext(BackendContext)
  if (!backend) throw new Error('useBackend: no BackendProvider above this component')
  return backend
}
