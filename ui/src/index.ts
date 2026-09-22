// The ObSink screens and the seam they run on. desktop/ and web/ each provide
// a `Backend` and render `PopoverApp` / `SettingsApp` inside a
// `BackendProvider`.
export { BackendProvider, useBackend } from './backend'
export type {
  Backend,
  BackendEvent,
  BackendEvents,
  CreateVaultRequest,
  DownloadVaultRequest,
  Platform,
  Resolution,
} from './backend'
export { PopoverApp } from './popover/PopoverApp'
export { SettingsApp } from './settings/SettingsApp'
export { isInviteRequired, isUnauthorized, SESSION_EXPIRED, toCommandError } from './lib/errors'
export type * from './types'
