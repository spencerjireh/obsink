// Shapes shared between the Tauri commands (see src-tauri/src/main.rs) and
// the components. Field names mirror the Rust structs (snake_case).

export type SyncAction = {
  path: string
  kind: 'Upload' | 'Download' | 'DeleteLocal' | 'DeleteRemote'
}

export type Conflict = {
  path: string
  local: { modified: number; size: number; deleted?: boolean }
  remote: { modified: number; size: number; deleted?: boolean }
}

export type SyncFailure = {
  path: string
  kind: SyncAction['kind']
  error: string
  fatal: boolean
}

export type SyncResult = {
  upload: SyncAction[]
  download: SyncAction[]
  conflicts: Conflict[]
  failures: SyncFailure[]
}

// Core `ProgressEvent` serializes (serde, externally-tagged) to this shape.
export type SyncPhase = 'Downloading' | 'ResolvingConflicts' | 'Uploading'

export type ProgressEvent =
  | { Phase: SyncPhase }
  | { FileStarted: { path: string; kind: SyncAction['kind']; index: number; total: number } }
  | { FileCompleted: { path: string; bytes: number } }
  | { FileFailed: { path: string; error: string } }
  | { Done: { uploaded: number; downloaded: number; failed: number } }

export type Progress = {
  phase: SyncPhase
  current: number
  total: number
  path: string | null
}

export type SyncStatus = {
  active_vault_id: string | null
  configured_vaults: number
  pending_uploads: number
  pending_downloads: number
  pending_conflicts: number
  last_sync_manifest_path: string | null
}

export type LocalVault = {
  id: string
  name: string
  server_url: string
  local_path: string
  active: boolean
}

export type SyncResponse = {
  completed_result: SyncResult | null
  pending_conflicts: Conflict[]
}

export type ConflictPreview = {
  path: string
  local_text: string
  remote_text: string
  local_deleted: boolean
  remote_deleted: boolean
}

export type AddVaultMode = 'create' | 'connect'
export type ResolutionChoice = 'KeepLocal' | 'KeepRemote' | 'KeepBoth'

export type AddVaultForm = {
  mode: AddVaultMode
  local_path: string
  vault_name: string
  vault_id: string
  passphrase: string
}

export type UsageInfo = {
  total_bytes: number
  max_vault_bytes: number | null
  max_vaults: number | null
  vaults: { id: string; bytes: number }[]
}

export type AccountState =
  | { kind: 'signed_out' }
  | {
      kind: 'account'
      user_id: string
      email: string | null
      devices: { session_id: string; device_name: string; current: boolean }[]
      usage: UsageInfo | null
    }

export type InviteInfo = { code: string; expires: number }

export type RemoteVault = { id: string; name: string; created: number }

// Which pane the main area shows. Setup replaces the vault pane; the sidebar
// stays so the user can always get back.
export type View = 'vault' | 'setup'
export type SetupSection = 'account' | 'add-vault'
// A sidebar request to scroll the setup view to one section; `at` makes a
// repeat click on the same button scroll again.
export type SetupFocus = { section: SetupSection; at: number }
