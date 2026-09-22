import type {
  AccountState,
  AuthCapabilities,
  DeviceInfo,
  DevicePlatform,
  InviteInfo,
  ProtocolInfo,
  SetPassphraseOutcome,
} from '@obsink/ui'
import type { AccountKeyMaterial, Me, WireDevice, WireInvite } from './api'
import { all, type StoredVault } from '../shared/db'
import { stopDriver } from './driver'
import { other } from './errors'
import {
  accountKey,
  createAccountKey,
  forgetAccountKey,
  rememberAccountKey,
  unlockAccountKey,
} from './keys'
import {
  api,
  bearerCall,
  deviceName,
  forgetBearer,
  loadBearer,
  saveBearer,
  serverUrl,
} from './session'
import { forgetVaultsForServer, unlockStoredVaults } from './vaults'
import { wasm } from './wasm'

// The account commands, one per desktop `#[tauri::command]`.

export async function getAuthCapabilities(): Promise<AuthCapabilities> {
  const caps = await api.capabilities()
  return {
    email: caps.auth.email,
    apple: caps.auth.apple,
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
  const session = await api.emailVerify(email.trim(), code.trim(), deviceName(), invite)
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
  rememberAccountKey(key, id)
  await unlockStoredVaults()
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
  await forgetVaultsForServer(serverUrl)
}
