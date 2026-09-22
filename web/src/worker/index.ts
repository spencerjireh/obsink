import type { WorkerRequest, WorkerResponse } from '../shared/protocol'
import * as account from './account'
import { listActivity } from './activity'
import { emit, stateChanged } from './bus'
import { getConflictPreview, resolveConflict, setPaused, syncVault } from './driver'
import { asCommandError } from './errors'
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
  createInvite: account.createInvite,
  listInvites: account.listInvites,
  revokeDevice: account.revokeDevice,
  signOut: account.signOut,
  deleteAccount: account.deleteAccount,
  listVaults: vaults.listVaults,
  createVault: vaults.createVault,
  downloadVault: vaults.downloadVault,
  removeVault: vaults.removeVault,
  deleteRemoteVault: vaults.deleteRemoteVault,
  syncVault,
  resolveConflict,
  getConflictPreview,
  listActivity,
  setVisibility: (hidden: boolean) => setPaused(hidden),
}

// Methods that change what the screens show; the worker announces it the
// way desktop emits `state://changed` after every mutating command.
const CHANGES_STATE = new Set([
  'authEmailVerify',
  'setPassphrase',
  'unlock',
  'signOut',
  'deleteAccount',
  'createVault',
  'downloadVault',
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
    const vaultId = typeof args[0] === 'string' && method !== 'authEmailVerify' ? args[0] : null
    stateChanged(vaultId)
  }
}

// A throw outside a request handler (a timer, a poll) has no request to
// answer; the page shows it instead of nothing.
self.addEventListener('error', (event) => {
  emit('client://error', { message: event.message || 'The browser client hit an error.' })
})
self.addEventListener('unhandledrejection', (event) => {
  emit('client://error', { message: asCommandError(event.reason).message })
})
