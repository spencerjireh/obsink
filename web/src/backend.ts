import type { Backend, BackendEvent, BackendEvents, CommandError, SettingsTarget } from '@obsink/ui'
import { get, put, type StoredVault } from './shared/db'
import { isResponse, type WorkerMessage, type WorkerRequest } from './shared/protocol'

// The browser Backend. Everything that touches the server, the folder or the
// keys runs in the worker (`./worker`), so Argon2 and a vault scan never
// block the page; this side is the RPC client plus the two things only the
// page may do: pick a folder and ask for permission on one (user gestures).
export class WebBackend implements Backend {
  readonly platform = {
    kind: 'web' as const,
    deviceNoun: 'this browser',
    keyStoreNoun: 'this browser',
    canOpenFolder: false,
    folderPlaceholder: '',
  }

  private worker: Worker
  private nextId = 1
  private pending = new Map<
    number,
    { resolve: (value: unknown) => void; reject: (error: CommandError) => void }
  >()
  private handlers = new Map<BackendEvent, Set<(payload: unknown) => void>>()

  constructor() {
    this.worker = new Worker(new URL('./worker/index.ts', import.meta.url), { type: 'module' })
    this.worker.onmessage = (message: MessageEvent<WorkerMessage>) => {
      const data = message.data
      if (isResponse(data)) {
        const waiter = this.pending.get(data.id)
        if (!waiter) return
        this.pending.delete(data.id)
        if (data.ok) waiter.resolve(data.value)
        else waiter.reject(data.error)
        return
      }
      this.emit(data.event, data.payload)
    }
    this.worker.onerror = (event) => {
      const error: CommandError = { kind: 'other', message: event.message || 'worker failed' }
      for (const waiter of this.pending.values()) waiter.reject(error)
      this.pending.clear()
    }
  }

  private call<T>(method: string, ...args: unknown[]): Promise<T> {
    const id = this.nextId++
    const request: WorkerRequest = { id, method, args }
    return new Promise<T>((resolve, reject) => {
      this.pending.set(id, { resolve: resolve as (value: unknown) => void, reject })
      this.worker.postMessage(request)
    })
  }

  private emit<E extends BackendEvent>(event: E, payload: BackendEvents[E]) {
    for (const handler of this.handlers.get(event) ?? []) handler(payload)
  }

  on<E extends BackendEvent>(event: E, handler: (payload: BackendEvents[E]) => void) {
    const set = this.handlers.get(event) ?? new Set()
    const typed = handler as (payload: unknown) => void
    set.add(typed)
    this.handlers.set(event, set)
    return () => {
      set.delete(typed)
    }
  }

  getServerUrl = () => this.call<string>('getServerUrl')
  getAuthCapabilities = () => this.call<never>('getAuthCapabilities')
  authEmailStart = (email: string) => this.call<string | null>('authEmailStart', email)
  authEmailVerify = (email: string, code: string, inviteCode: string | null) =>
    this.call<never>('authEmailVerify', email, code, inviteCode)
  getAccount = () => this.call<never>('getAccount')
  createInvite = () => this.call<never>('createInvite')
  listInvites = () => this.call<never>('listInvites')
  revokeSession = (sessionId: string) => this.call<never>('revokeSession', sessionId)
  signOut = () => this.call<void>('signOut')
  deleteAccount = () => this.call<void>('deleteAccount')
  listRemoteVaults = () => this.call<never>('listRemoteVaults')
  addVault = (request: Parameters<Backend['addVault']>[0]) => this.call<never>('addVault', request)
  removeVault = (vaultId: string) => this.call<void>('removeVault', vaultId)
  deleteRemoteVault = (vaultId: string) => this.call<void>('deleteRemoteVault', vaultId)
  getVaultStates = () => this.call<never>('getVaultStates')
  syncVault = (vaultId: string) => this.call<never>('syncVault', vaultId)
  resolveConflict = (vaultId: string, resolutions: Parameters<Backend['resolveConflict']>[1]) =>
    this.call<never>('resolveConflict', vaultId, resolutions)
  getConflictPreview = (vaultId: string, path: string) =>
    this.call<never>('getConflictPreview', vaultId, path)
  listActivity = (vaultId: string | null, limit: number) =>
    this.call<never>('listActivity', vaultId, limit)
  unlockVault = (vaultId: string, passphrase: string) =>
    this.call<void>('unlockVault', vaultId, passphrase)

  // A directory picker is a user gesture on the page; the handle goes to
  // IndexedDB under a fresh id that `addVault` receives as `local_path`.
  async pickFolder() {
    const handle = await window.showDirectoryPicker({ mode: 'readwrite' })
    const id = crypto.randomUUID()
    await put('handles', id, handle)
    return { id, name: handle.name }
  }

  // Chrome forgets the grant per session: ask again from a click.
  async requestFolderAccess(vaultId: string) {
    const vault = await get<StoredVault>('vaults', vaultId)
    const handle = vault && (await get<FileSystemDirectoryHandle>('handles', vault.handle_id))
    if (!handle)
      throw { kind: 'other', message: 'Folder not found. Remove the vault and add it again.' }
    const state = await handle.requestPermission({ mode: 'readwrite' })
    if (state !== 'granted') throw { kind: 'other', message: 'Folder access was not granted.' }
    this.emit('state://changed', { vault_id: vaultId })
  }

  openVaultFolder() {
    return Promise.reject<void>({ kind: 'other', message: 'Not available in the browser.' })
  }

  // One window here: navigation is local.
  openSettings(target: Partial<SettingsTarget> = {}) {
    this.emit('settings://navigate', { tab: 'vaults', vault_id: null, add_vault: false, ...target })
    return Promise.resolve()
  }
}
