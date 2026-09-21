import { wasm, type VaultKeys } from './wasm'

// Derived keys live only here, in worker memory, for the tab's lifetime.
// A reload asks for the passphrase again (the vault shows `no_key` until
// then), which is the browser's version of "the key is in the keychain".
const keys = new Map<string, VaultKeys>()

export async function deriveKeys(vaultId: string, passphrase: string): Promise<VaultKeys> {
  const core = await wasm()
  return core.VaultKeys.derive(passphrase, vaultId)
}

export function rememberKeys(vaultId: string, vaultKeys: VaultKeys): void {
  forgetKeys(vaultId)
  keys.set(vaultId, vaultKeys)
}

export function keysFor(vaultId: string): VaultKeys | null {
  return keys.get(vaultId) ?? null
}

export function forgetKeys(vaultId: string): void {
  keys.get(vaultId)?.free()
  keys.delete(vaultId)
}
