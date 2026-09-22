import type { Backend, BackendEvent, BackendEvents } from '../backend'
import type { AccountState, VaultStateInfo } from '../types'

// A Backend for the unit tests: every method resolves to what the test
// programmed, events fire through `emit`, and calls are recorded.
export type MockBackend = Backend & {
  emit<E extends BackendEvent>(event: E, payload: BackendEvents[E]): void
  calls: { method: string; args: unknown[] }[]
}

export function vault(overrides: Partial<VaultStateInfo> = {}): VaultStateInfo {
  return {
    id: 'vault_1',
    name: 'Notes',
    local_path: '/Users/me/Notes',
    state: { kind: 'up_to_date' },
    last_synced: 1_700_000_000,
    revision: 3,
    bytes: 1024,
    devices: [],
    ...overrides,
  }
}

export function unlockedAccount(): AccountState {
  return {
    kind: 'account',
    user_id: 'usr_1',
    email: 'me@example.com',
    devices: [],
    usage: null,
  }
}

export function mockBackend(overrides: Partial<Backend> = {}): MockBackend {
  const handlers = new Map<BackendEvent, Set<(payload: unknown) => void>>()
  const calls: { method: string; args: unknown[] }[] = []
  const notImplemented = (method: string) => () => {
    calls.push({ method, args: [] })
    return Promise.reject({ kind: 'other', message: `${method} is not mocked` })
  }
  const backend: MockBackend = {
    platform: {
      kind: 'desktop',
      deviceNoun: 'this Mac',
      keyStoreNoun: 'the keychain',
      canOpenFolder: true,
      folderPlaceholder: '/Users/you/Documents/Notes',
      folderPrompt: {
        create: 'Where the vault lives on this Mac',
        download: 'Where to put the vault on this Mac',
      },
    },
    calls,
    getServerUrl: () => Promise.resolve('https://server.test'),
    getProtocol: () => Promise.resolve({ server: 3, client: 3 }),
    getAuthCapabilities: () =>
      Promise.resolve({ email: true, apple: false, invite_required: false }),
    authEmailStart: notImplemented('authEmailStart'),
    authEmailVerify: notImplemented('authEmailVerify'),
    getAccount: () => Promise.resolve(unlockedAccount()),
    setPassphrase: notImplemented('setPassphrase'),
    unlock: notImplemented('unlock'),
    changePassphrase: notImplemented('changePassphrase'),
    createInvite: notImplemented('createInvite'),
    listInvites: () => Promise.resolve([]),
    revokeDevice: notImplemented('revokeDevice'),
    renameDevice: notImplemented('renameDevice'),
    signOut: notImplemented('signOut'),
    deleteAccount: notImplemented('deleteAccount'),
    listVaults: () => Promise.resolve([]),
    createVault: notImplemented('createVault'),
    downloadVault: notImplemented('downloadVault'),
    renameVault: notImplemented('renameVault'),
    removeVault: notImplemented('removeVault'),
    deleteRemoteVault: notImplemented('deleteRemoteVault'),
    syncVault: notImplemented('syncVault'),
    resolveConflict: notImplemented('resolveConflict'),
    getConflictPreview: notImplemented('getConflictPreview'),
    listActivity: () => Promise.resolve([]),
    openVaultFolder: () => Promise.resolve(),
    openSettings: () => Promise.resolve(),
    on(event, handler) {
      const set = handlers.get(event) ?? new Set()
      set.add(handler as (payload: unknown) => void)
      handlers.set(event, set)
      return () => {
        set.delete(handler as (payload: unknown) => void)
      }
    },
    emit(event, payload) {
      for (const handler of handlers.get(event) ?? []) handler(payload)
    },
    ...overrides,
  }
  // Record every call so a test can assert what the screens asked for.
  for (const key of Object.keys(overrides) as (keyof Backend)[]) {
    const original = backend[key]
    if (typeof original !== 'function' || key === 'on') continue
    ;(backend as unknown as Record<string, unknown>)[key] = (...args: unknown[]) => {
      calls.push({ method: key, args })
      return (original as (...a: unknown[]) => unknown).apply(backend, args)
    }
  }
  return backend
}
