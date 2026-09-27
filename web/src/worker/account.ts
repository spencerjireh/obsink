import type {
  AccountState,
  ApprovalRequest,
  AuthCapabilities,
  DeviceInfo,
  DevicePlatform,
  InviteInfo,
  ProtocolInfo,
  SetPassphraseOutcome,
} from '@obsink/ui'
import type { AccountKeyMaterial, Me, WireDevice, WireInvite } from './api'
import { all, type StoredVault } from '../shared/db'
import { stateChanged } from './bus'
import { stopDriver } from './driver'
import { other } from './errors'
import {
  accountKey,
  accountKeyFromBytes,
  createAccountKey,
  forgetAccountKey,
  forgetPendingApproval,
  newApprovalRequest,
  pendingApproval,
  rememberAccountKey,
  rememberPendingApproval,
  unlockAccountKey,
  type PendingApproval,
} from './keys'
import {
  api,
  bearerCall,
  forgetBearer,
  loadBearer,
  loadOrCreateDeviceId,
  saveBearer,
  thisDevice,
} from './session'
import { forgetAllVaults, unlockStoredVaults } from './vaults'
import { wasm } from './wasm'

// The account commands, one per desktop `#[tauri::command]`.

export async function getAuthCapabilities(): Promise<AuthCapabilities> {
  const caps = await api.capabilities()
  return {
    email: caps.auth.email,
    invite_required: caps.invite_required ?? false,
  }
}

// Spec §15.5: the server's wire format against this build's.
export async function getProtocol(): Promise<ProtocolInfo> {
  const core = await wasm()
  const client = core.protocolVersion()
  try {
    const caps = await api.capabilities()
    return { server: caps.protocol ?? 0, client }
  } catch {
    return { server: null, client }
  }
}

// Returns the code itself only against a dev server (`AUTH_DEV_RETURN_CODE=1`).
export async function authEmailStart(email: string): Promise<string | null> {
  const result = await api.emailStart(email.trim())
  return result.code ?? null
}

export async function authEmailVerify(
  email: string,
  code: string,
  inviteCode: string | null,
): Promise<AccountState> {
  const invite = inviteCode?.trim() || null
  const session = await api.emailVerify(email.trim(), code.trim(), await thisDevice(), invite)
  await saveBearer(session.token)
  return getAccount()
}

function inviteInfo(invite: WireInvite): InviteInfo {
  const status = invite.status === 'used' || invite.status === 'expired' ? invite.status : 'active'
  return {
    code: invite.code,
    created: invite.created ?? 0,
    expires: invite.expires,
    status,
    used_at: invite.used_at ?? null,
  }
}

function platformOf(value: string): DevicePlatform {
  return value === 'macos' || value === 'ios' || value === 'browser' || value === 'cli'
    ? value
    : 'unknown'
}

function deviceInfo(device: WireDevice): DeviceInfo {
  return {
    id: device.id,
    name: device.name,
    platform: platformOf(device.platform),
    created: device.created,
    last_seen: device.last_seen ?? device.created,
    current: device.current,
    vault_ids: device.vault_ids ?? [],
    approval: device.approval
      ? { requested: device.approval.requested, expires: device.approval.expires }
      : null,
  }
}

function accountOf(me: Me & { user: NonNullable<Me['user']> }): AccountState {
  return {
    kind: 'account',
    user_id: me.user.id,
    email: me.user.email,
    devices: (me.devices ?? []).map(deviceInfo),
    usage: me.usage
      ? {
          total_bytes: me.usage.total_bytes,
          max_vault_bytes: me.usage.max_vault_bytes,
          max_vaults: me.usage.max_vaults,
          vaults: (me.usage.vaults ?? []).map((vault) => ({ id: vault.id, bytes: vault.bytes })),
        }
      : null,
  }
}

// Signed out, locked (the key is not in this worker), or unlocked.
export async function getAccount(): Promise<AccountState> {
  const bearer = await loadBearer()
  if (!bearer) return { kind: 'signed_out' }
  try {
    const me = await api.me(bearer)
    if (!me.user) return { kind: 'signed_out' }
    if (!accountKey()) {
      const blob = await api.getKeys(bearer)
      return { kind: 'locked', user_id: me.user.id, email: me.user.email, has_key: blob !== null }
    }
    return accountOf({ ...me, user: me.user })
  } catch (error) {
    if ((error as { kind?: string })?.kind === 'unauthorized') {
      // Session revoked/expired elsewhere: signed out is the state.
      await forgetBearer()
      forgetAccountKey()
      return { kind: 'signed_out' }
    }
    throw error
  }
}

async function signedInUser(): Promise<{ bearer: string; id: string }> {
  const bearer = await loadBearer()
  if (!bearer) throw other('Sign in first.')
  const me = await api.me(bearer)
  if (!me.user) throw other('Sign in first.')
  return { bearer, id: me.user.id }
}

// Spec §12.1: set the passphrase (create-only); on a lost race, unlock the
// winner's key with the same passphrase instead.
export async function setPassphrase(
  passphrase: string,
): Promise<{ outcome: SetPassphraseOutcome; account: AccountState }> {
  const { bearer, id } = await signedInUser()
  const created = await createAccountKey(passphrase, id)
  const material = JSON.parse(created.material()) as {
    wrapped: string
    salt: string
    verifier: string
  }
  const result = await api.setKeys(bearer, material)
  if (result.outcome === 'created') {
    rememberAccountKey(created, id)
    await unlockStoredVaults()
    return { outcome: 'created', account: await getAccount() }
  }
  created.free()
  const unlocked = await unlockAccountKey(
    passphrase,
    result.blob.salt,
    result.blob.wrapped,
    id,
  ).catch(() => null)
  if (!unlocked) {
    // The form turns into Unlock with the race notice.
    throw {
      kind: 'server',
      status: 409,
      message: 'A passphrase was already set on another device. Enter it.',
    }
  }
  rememberAccountKey(unlocked, id)
  await unlockStoredVaults()
  return { outcome: 'exists', account: await getAccount() }
}

export async function unlock(passphrase: string): Promise<AccountState> {
  const { bearer, id } = await signedInUser()
  const blob = await api.getKeys(bearer)
  if (!blob) throw other('Set a passphrase first.')
  let key
  try {
    key = await unlockAccountKey(passphrase, blob.salt, blob.wrapped, id)
  } catch {
    throw other('Passphrase does not match this account.')
  }
  const withdraw = pendingApproval() !== null
  rememberAccountKey(key, id)
  await unlockStoredVaults()
  // The passphrase won: other devices stop listing this one as waiting.
  if (withdraw) await api.clearApproval(bearer).catch(() => undefined)
  return getAccount()
}

// DESIGN.md §5 copy for the approver (spec §15.3).
const FINGERPRINT_MISMATCH = 'Fingerprint does not match. Check it on the other device.'
const APPROVAL_EXPIRED = 'Approval expired. The other device shows a new fingerprint.'

function nowSeconds(): number {
  return Math.floor(Date.now() / 1000)
}

// A fresh keypair registered as this device's request (replacing any
// earlier one on the server); the secret stays in the worker.
async function registerApproval(bearer: string, deviceId: string): Promise<PendingApproval> {
  const request = await newApprovalRequest()
  let registered
  try {
    registered = await api.registerApproval(bearer, request.publicKey())
  } catch (error) {
    request.free()
    throw error
  }
  const next = { request, deviceId, expires: registered.expires }
  rememberPendingApproval(next)
  return next
}

function isLive(pending: PendingApproval | null, deviceId: string): pending is PendingApproval {
  return pending !== null && pending.deviceId === deviceId && pending.expires > nowSeconds()
}

type ApprovalPoll = { approval: ApprovalRequest | null; account: AccountState }

// Spec §12.1, the waiting device: one tick of the loop the unlock form runs
// while the account is locked with a passphrase set. Registers a request
// when none is live, polls it, and on the wrapped key unlocks like `unlock`
// does. It announces the state itself, only when it unlocked; every other
// tick is a read. Ticks can overlap (the form's first tick and its
// interval, a slow server): one runs at a time and an overlapping caller
// shares its answer, so two ticks never register two keypairs and show a
// fingerprint the server no longer holds.
let polling: Promise<ApprovalPoll> | null = null

export function pollApproval(): Promise<ApprovalPoll> {
  if (!polling) {
    polling = pollApprovalOnce().finally(() => {
      polling = null
    })
  }
  return polling
}

async function pollApprovalOnce(): Promise<ApprovalPoll> {
  const account = await getAccount()
  if (account.kind !== 'locked' || !account.has_key) {
    forgetPendingApproval()
    return { approval: null, account }
  }
  try {
    return await bearerCall(async (bearer) => {
      const deviceId = await loadOrCreateDeviceId()
      let pending = pendingApproval()
      if (!isLive(pending, deviceId)) pending = await registerApproval(bearer, deviceId)
      let status = await api.approvalStatus(bearer)
      // Expired, withdrawn, or not ours (the row was reset): start over, so
      // the fingerprint changes.
      if (!status || status.public_key !== pending.request.publicKey()) {
        pending = await registerApproval(bearer, deviceId)
        status = null
      }
      if (status?.wrapped) {
        let raw: Uint8Array
        try {
          raw = pending.request.accept(status.wrapped, account.user_id, deviceId)
        } catch {
          // A blob this request cannot open (wrapped to an earlier key of
          // this device): start over rather than fail on every tick.
          pending = await registerApproval(bearer, deviceId)
          return {
            approval: { fingerprint: pending.request.fingerprint(), expires: pending.expires },
            account,
          }
        }
        return { approval: null, account: await acceptApproval(bearer, raw, account.user_id) }
      }
      return {
        approval: {
          fingerprint: pending.request.fingerprint(),
          expires: status?.expires ?? pending.expires,
        },
        account,
      }
    })
  } catch (error) {
    if ((error as { kind?: string })?.kind === 'unauthorized') forgetPendingApproval()
    throw error
  }
}

// The account key arrived: keep it the way `unlock` does, withdraw the
// request, and tell the page.
async function acceptApproval(
  bearer: string,
  raw: Uint8Array,
  userId: string,
): Promise<AccountState> {
  const key = await accountKeyFromBytes(raw, userId)
  raw.fill(0)
  rememberAccountKey(key, userId)
  await unlockStoredVaults()
  await api.clearApproval(bearer).catch(() => undefined)
  forgetPendingApproval()
  stateChanged(null)
  return getAccount()
}

// Spec §12.3, the approver: the fingerprint typed here must be the one of
// the public key `GET /auth/me` relays for that device; the wasm side
// refuses a mismatch before anything is wrapped, and nothing is sent.
export async function approveDevice(deviceId: string, fingerprint: string): Promise<AccountState> {
  const typed = fingerprint.replace(/[\s-]/g, '').toUpperCase()
  const key = accountKey()
  if (!key) throw other('Unlock the account first.')
  await bearerCall(async (bearer) => {
    const me = await api.me(bearer)
    const publicKey = me.devices?.find((device) => device.id === deviceId)?.approval?.public_key
    if (!publicKey) throw other(APPROVAL_EXPIRED)
    let body: { wrapped: string; verifier: string }
    try {
      body = JSON.parse(key.approveDevice(deviceId, publicKey, typed)) as typeof body
    } catch (error) {
      const message = error instanceof Error ? error.message : String(error)
      if (message.includes('fingerprint')) throw other(FINGERPRINT_MISMATCH)
      throw error
    }
    try {
      await api.approveDevice(bearer, deviceId, body.wrapped, body.verifier)
    } catch (error) {
      const failure = error as { kind?: string; status?: number }
      if (failure.kind === 'server' && failure.status === 404) throw other(APPROVAL_EXPIRED)
      throw error
    }
  })
  return getAccount()
}

// DESIGN.md §5 `Change passphrase`: the current one must open the stored
// blob; the key itself is unchanged, rewrapped under the new KEK.
export async function changePassphrase(current: string, next: string): Promise<void> {
  const { bearer, id } = await signedInUser()
  const key = accountKey()
  if (!key) throw other('Unlock the account first.')
  const blob = await api.getKeys(bearer)
  if (!blob) throw other('Set a passphrase first.')
  let check
  try {
    check = await unlockAccountKey(current, blob.salt, blob.wrapped, id)
  } catch {
    throw other('Passphrase does not match this account.')
  }
  check.free()
  const material = JSON.parse(key.rewrap(next)) as AccountKeyMaterial
  await api.rewrapKeys(bearer, material)
}

export function createInvite(): Promise<InviteInfo> {
  return bearerCall(async (bearer) => inviteInfo(await api.createInvite(bearer)))
}

export function listInvites(): Promise<InviteInfo[]> {
  return bearerCall(async (bearer) => (await api.listInvites(bearer)).map(inviteInfo))
}

export async function revokeDevice(deviceId: string): Promise<AccountState> {
  await bearerCall((bearer) => api.revokeDevice(bearer, deviceId))
  return getAccount()
}

export async function renameDevice(deviceId: string, name: string): Promise<AccountState> {
  const trimmed = name.trim()
  if (!trimmed) throw other('Enter a device name.')
  await bearerCall((bearer) => api.renameDevice(bearer, deviceId, trimmed))
  return getAccount()
}

export async function signOut(): Promise<void> {
  const bearer = await loadBearer()
  if (bearer) {
    try {
      await api.logout(bearer)
    } catch {
      // Best effort: the bearer is forgotten either way.
    }
  }
  await forgetBearer()
  // Nothing keeps polling, and no key stays in memory, for an account that
  // is signed out; the vault entries stay so signing in again resumes.
  for (const vault of await all<StoredVault>('vaults')) {
    stopDriver(vault.id)
  }
  forgetAccountKey()
}

export async function deleteAccount(): Promise<void> {
  await bearerCall((bearer) => api.deleteAccount(bearer))
  await forgetBearer()
  forgetAccountKey()
  await forgetAllVaults()
}
