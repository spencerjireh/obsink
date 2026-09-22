// Shapes shared between the Tauri commands (see src-tauri/src/main.rs), the
// browser worker, and the components. Field names mirror the Rust structs
// (snake_case).

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
  checkpoint_error?: string
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

// What one vault row shows (spec §15.1). The first block is a vault this
// device holds; the last three are the account's vaults as this device sees
// them.
export type VaultState =
  | { kind: 'up_to_date' }
  | { kind: 'pending'; uploads: number; downloads: number }
  | { kind: 'conflicts'; count: number; awaiting_resolution: boolean }
  | { kind: 'syncing' }
  | { kind: 'error'; error_kind: CommandErrorKind; message: string }
  // Browser only: the folder's permission grant lapsed (Chrome forgets it
  // per session); a click on the vault page asks again.
  | { kind: 'needs_access' }
  // The account owns it; this device does not hold it. `Download` on the row.
  | { kind: 'not_on_device' }
  // This device holds a folder for a vault the server no longer lists.
  | { kind: 'deleted_on_server' }
  // The account key is not available (browser after a reload, or a lost
  // first-set race); unlock first.
  | { kind: 'locked' }

// A device that holds a vault, as the server reports it.
export type VaultDevice = {
  id: string
  name: string
  platform: DevicePlatform
  last_synced: number | null
  last_revision: number | null
}

export type VaultStateInfo = {
  id: string
  name: string
  // Null for a vault this device does not hold.
  local_path: string | null
  state: VaultState
  last_synced: number | null
  // The server's manifest revision and live bytes (0 when the server did
  // not answer).
  revision: number
  bytes: number
  devices: VaultDevice[]
}

// `sync://progress` payload: the core event tagged with its vault.
export type ProgressEnvelope = { vault_id: string; event: ProgressEvent }

// A vault this device holds, as `createVault` / `downloadVault` return it.
export type LocalVault = {
  id: string
  name: string
  local_path: string
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

export type ResolutionChoice = 'KeepLocal' | 'KeepRemote' | 'KeepBoth'

// Bytes one vault uses on the server against the per-vault cap (null when
// the server sets none).
export type VaultUsage = { bytes: number; max: number | null }

export type UsageInfo = {
  total_bytes: number
  max_vault_bytes: number | null
  max_vaults: number | null
  vaults: { id: string; bytes: number }[]
}

export type DevicePlatform = 'macos' | 'ios' | 'browser' | 'cli' | 'unknown'

// A device of the account (`GET /auth/me`, spec §4.1).
export type DeviceInfo = {
  id: string
  name: string
  platform: DevicePlatform
  created: number
  last_seen: number
  current: boolean
  vault_ids: string[]
}

// Signed out; signed in but the account key is not at hand (`has_key` says
// whether the account has a passphrase to enter or needs one set); or fully
// unlocked.
export type AccountState =
  | { kind: 'signed_out' }
  | { kind: 'locked'; user_id: string; email: string | null; has_key: boolean }
  | {
      kind: 'account'
      user_id: string
      email: string | null
      devices: DeviceInfo[]
      usage: UsageInfo | null
    }

// What `setPassphrase` did: this device set it, or another device had
// already set it and the passphrase given unlocked that key instead.
export type SetPassphraseOutcome = 'created' | 'exists'

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

// The wire format the server speaks against the one this build speaks
// (spec §15.5); `server` is null when the server did not answer.
export type ProtocolInfo = { server: number | null; client: number }

// The settings window's tabs, and what the popover asks it to show
// (`settings://navigate`, Rust `SettingsTarget`).
export type SettingsTab = 'vaults' | 'account' | 'activity'
export type SettingsTarget = {
  tab: SettingsTab
  vault_id: string | null
  // Open the Create vault flow.
  add_vault: boolean
  // Open the Download flow for `vault_id`.
  download?: boolean
}

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
