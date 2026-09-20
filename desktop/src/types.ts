// Shapes shared between the Tauri commands (see src-tauri/src/main.rs) and
// the components. Field names mirror the Rust structs (snake_case).

// Every command rejects with this (Rust `CommandError`). `status` is set for
// `server` errors so a 403 can be told apart without parsing the message.
export type CommandErrorKind = 'unauthorized' | 'network' | 'server' | 'other'
export type CommandError = { kind: CommandErrorKind; message: string; status?: number }

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

// Bytes one vault uses on the server against the per-vault cap (null when
// the server sets none).
export type VaultUsage = { bytes: number; max: number | null }

export type UsageInfo = {
  total_bytes: number
  max_vault_bytes: number | null
  max_vaults: number | null
  vaults: { id: string; bytes: number }[]
}

export type DeviceInfo = {
  session_id: string
  device_name: string
  created: number
  current: boolean
}

export type AccountState =
  | { kind: 'signed_out' }
  | {
      kind: 'account'
      user_id: string
      email: string | null
      devices: DeviceInfo[]
      usage: UsageInfo | null
    }

export type InviteStatus = 'active' | 'used' | 'expired'

export type InviteInfo = {
  code: string
  created: number
  expires: number
  status: InviteStatus
  used_at: number | null
}

// `GET /` on the server: which sign-in methods exist and whether a new
// account needs an invite code.
export type AuthCapabilities = { email: boolean; apple: boolean; invite_required: boolean }

export type RemoteVault = { id: string; name: string; created: number; max_file_size: number }

// Which pane the main area shows. Setup replaces the vault pane; the sidebar
// stays so the user can always get back.
export type View = 'vault' | 'setup'
export type SetupSection = 'account' | 'add-vault'
// A sidebar request to scroll the setup view to one section; `at` makes a
// repeat click on the same button scroll again.
export type SetupFocus = { section: SetupSection; at: number }

// One line of the per-vault activity log (Rust `activity::ActivityEvent`),
// newest first as returned by `list_activity`.
export type ActivityKind =
  'uploaded' | 'downloaded' | 'deleted_here' | 'deleted_on_server' | 'conflict' | 'error' | 'synced'
export type ActivityEvent = {
  at: number
  vault_id: string
  kind: ActivityKind
  path?: string
  detail?: string
}
