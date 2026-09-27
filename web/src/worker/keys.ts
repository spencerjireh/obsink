import { wasm, type AccountKey, type ApprovalRequest, type VaultKeys } from './wasm'

// Keys live only here, in worker memory, for the tab's lifetime: the
// account key once the passphrase was entered (spec §6.3, browser row) and
// one `VaultKeys` per vault this browser holds, unwrapped from the member
// blob `GET /vaults` returns. A reload asks for the passphrase again (the
// account shows `locked` until then).

let account: { key: AccountKey; userId: string } | null = null
const vaults = new Map<string, VaultKeys>()

// Spec §12.1: this device's live approval request while it waits for the
// key. The secret lives only in the wasm handle, so a reload starts a new
// request (spec §6.3, browser row); it goes with the account key.
export type PendingApproval = { request: ApprovalRequest; deviceId: string; expires: number }
let pending: PendingApproval | null = null

export function accountKey(): AccountKey | null {
  return account?.key ?? null
}

export function accountUserId(): string | null {
  return account?.userId ?? null
}

export function rememberAccountKey(key: AccountKey, userId: string): void {
  forgetAccountKey()
  account = { key, userId }
}

export function forgetAccountKey(): void {
  account?.key.free()
  account = null
  forgetPendingApproval()
  for (const vaultId of [...vaults.keys()]) forgetKeys(vaultId)
}

export function pendingApproval(): PendingApproval | null {
  return pending
}

export function rememberPendingApproval(next: PendingApproval): void {
  forgetPendingApproval()
  pending = next
}

// Frees the handle (the secret with it); idempotent.
export function forgetPendingApproval(): void {
  pending?.request.free()
  pending = null
}

// A fresh approval keypair for `PUT /auth/approval`.
export async function newApprovalRequest(): Promise<ApprovalRequest> {
  const core = await wasm()
  return core.ApprovalRequest.create()
}

// The account key as another device handed it over (spec §12.3).
export async function accountKeyFromBytes(raw: Uint8Array, userId: string): Promise<AccountKey> {
  const core = await wasm()
  return core.AccountKey.fromBytes(raw, userId)
}

// Set the passphrase for the first time: a fresh account key; the material
// for `PUT /auth/keys` is on the handle.
export async function createAccountKey(passphrase: string, userId: string): Promise<AccountKey> {
  const core = await wasm()
  return core.AccountKey.create(passphrase, userId)
}

// Unlock from what `GET /auth/keys` returned; throws on a wrong passphrase.
export async function unlockAccountKey(
  passphrase: string,
  salt: string,
  wrapped: string,
  userId: string,
): Promise<AccountKey> {
  const core = await wasm()
  return core.AccountKey.unlock(passphrase, salt, wrapped, userId)
}

// A fresh vault key, wrapped for this account: what `POST /vaults` sends.
export async function newWrappedVaultKey(
  vaultId: string,
): Promise<{ key: Uint8Array; wrapped: string }> {
  const core = await wasm()
  const key = core.newVaultKey()
  const holder = requireAccountKey()
  return { key, wrapped: holder.wrapVaultKey(key, vaultId) }
}

export function requireAccountKey(): AccountKey {
  const key = accountKey()
  if (!key) throw { kind: 'other', message: 'Unlock the account first.' }
  return key
}

// The vault's sub-keys from its member blob (`wrapped_key` of `GET /vaults`).
export async function unwrapVaultKeys(vaultId: string, wrapped: string): Promise<VaultKeys> {
  const core = await wasm()
  const raw = requireAccountKey().unwrapVaultKey(wrapped, vaultId)
  return core.VaultKeys.fromVaultKey(raw)
}

export async function keysFromRaw(raw: Uint8Array): Promise<VaultKeys> {
  const core = await wasm()
  return core.VaultKeys.fromVaultKey(raw)
}

export function rememberKeys(vaultId: string, vaultKeys: VaultKeys): void {
  forgetKeys(vaultId)
  vaults.set(vaultId, vaultKeys)
}

export function keysFor(vaultId: string): VaultKeys | null {
  return vaults.get(vaultId) ?? null
}

export function forgetKeys(vaultId: string): void {
  vaults.get(vaultId)?.free()
  vaults.delete(vaultId)
}
