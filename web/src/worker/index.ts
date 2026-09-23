import type { WorkerRequest, WorkerResponse } from '../shared/protocol'
import * as account from './account'
import { listActivity } from './activity'
import { emit, stateChanged } from './bus'
import { getConflictPreview, resolveConflict, setPaused, syncVault } from './driver'
import { asCommandError } from './errors'
import * as history from './history'
import { serverUrl } from './session'
import * as vaults from './vaults'

// The worker side of the Backend: one handler per method name, results and
// errors posted back by request id, events pushed as they happen.

// eslint-disable-next-line @typescript-eslint/no-explicit-any
const handlers: Record<string, (...args: any[]) => Promise<unknown> | unknown> = {
  getServerUrl: () => serverUrl,
  getProtocol: account.getProtocol,
  getAuthCapabilities: account.getAuthCapabilities,
  authEmailStart: account.authEmailStart,
  authEmailVerify: account.authEmailVerify,
  getAccount: account.getAccount,
  setPassphrase: account.setPassphrase,
  unlock: account.unlock,
  changePassphrase: account.changePassphrase,
  createInvite: account.createInvite,
  listInvites: account.listInvites,
  revokeDevice: account.revokeDevice,
  renameDevice: account.renameDevice,
  signOut: account.signOut,
  deleteAccount: account.deleteAccount,
  listVaults: vaults.listVaults,
  createVault: vaults.createVault,
  downloadVault: vaults.downloadVault,
  renameVault: vaults.renameVault,
  removeVault: vaults.removeVault,
  deleteRemoteVault: vaults.deleteRemoteVault,
  syncVault,
  resolveConflict,
  getConflictPreview,
  listActivity,
  setVisibility: (hidden: boolean) => setPaused(hidden),
  listFiles: history.listFiles,
  listVersions: history.listVersions,
  previewVersion: history.previewVersion,
  restoreVersion: history.restoreVersion,
  listTrash: history.listTrash,
  previewTrash: history.previewTrash,
  restoreTrash: history.restoreTrash,
}

// Methods that change what the screens show; the worker announces it the
// way desktop emits `state://changed` after every mutating command.
const CHANGES_STATE = new Set([
  'authEmailVerify',
  'setPassphrase',
  'unlock',
  'revokeDevice',
  'renameDevice',
  'signOut',
  'deleteAccount',
  'createVault',
  'downloadVault',
  'renameVault',
  'restoreVersion',
  'restoreTrash',
  'removeVault',
  'deleteRemoteVault',
])

// The methods whose first argument is the vault id (`state://changed` then
// names it).
const VAULT_METHODS = new Set([
  'renameVault',
  'restoreVersion',
  'restoreTrash',
  'removeVault',
  'deleteRemoteVault',
])

self.onmessage = async (message: MessageEvent<WorkerRequest>) => {
  const { id, method, args } = message.data
  let response: WorkerResponse
  try {
    const handler = handlers[method]
    if (!handler) throw new Error(`unknown backend method ${method}`)
    response = { id, ok: true, value: await handler(...args) }
  } catch (error) {
    response = { id, ok: false, error: asCommandError(error) }
  }
  self.postMessage(response)
  if (CHANGES_STATE.has(method)) {
    const vaultId = VAULT_METHODS.has(method) && typeof args[0] === 'string' ? args[0] : null
    stateChanged(vaultId)
  }
}

// Spec §15.5, the worker's side of the protocol gate: against a server that
// speaks another wire format nothing polls (the page shows `Update ObSink`).
void account.getProtocol().then((protocol) => {
  if (protocol.server !== null && protocol.server !== protocol.client) setPaused(true)
})

// A throw outside a request handler (a timer, a poll) has no request to
// answer; the page shows it instead of nothing.
self.addEventListener('error', (event) => {
  emit('client://error', { message: event.message || 'The browser client hit an error.' })
})
self.addEventListener('unhandledrejection', (event) => {
  emit('client://error', { message: asCommandError(event.reason).message })
})
