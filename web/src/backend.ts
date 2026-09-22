import type { Backend, BackendEvent, BackendEvents, CommandError, SettingsTarget } from '@obsink/ui'
import { del, get, put, type StoredVault } from './shared/db'
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
    folderPrompt: {
      create: 'Which folder on this computer holds the vault',
      download: 'Which folder on this computer to put the vault in',
    },
  }

  private worker: Worker
  private nextId = 1
  private pending = new Map<
    number,
    { resolve: (value: unknown) => void; reject: (error: CommandError) => void }
  >()
  private handlers = new Map<BackendEvent, Set<(payload: unknown) => void>>()
  // Set once the worker is gone: every later call fails at once instead of
  // waiting for an answer that never comes.
  private dead: CommandError | null = null
  // A folder picked in the add flow but not yet a vault.
  private pickedHandleId: string | null = null

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
    this.worker.onerror = (event) => this.die(event.message || 'The browser client stopped.')
    this.worker.onmessageerror = () => this.die('The browser client sent an unreadable message.')
  }

  private die(reason: string) {
    this.dead = { kind: 'other', message: `${reason} Reload the page.` }
    for (const waiter of this.pending.values()) waiter.reject(this.dead)
    this.pending.clear()
    this.emit('client://error', { message: this.dead.message })
  }

  private call<T>(method: string, ...args: unknown[]): Promise<T> {
    if (this.dead) return Promise.reject(this.dead)
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
  getProtocol = () => this.call<never>('getProtocol')
  getAuthCapabilities = () => this.call<never>('getAuthCapabilities')
  authEmailStart = (email: string) => this.call<string | null>('authEmailStart', email)
  authEmailVerify = (email: string, code: string, inviteCode: string | null) =>
    this.call<never>('authEmailVerify', email, code, inviteCode)
  getAccount = () => this.call<never>('getAccount')
  setPassphrase = (passphrase: string) => this.call<never>('setPassphrase', passphrase)
  unlock = (passphrase: string) => this.call<never>('unlock', passphrase)
  changePassphrase = (current: string, next: string) =>
    this.call<void>('changePassphrase', current, next)
  createInvite = () => this.call<never>('createInvite')
  listInvites = () => this.call<never>('listInvites')
  revokeDevice = (deviceId: string) => this.call<never>('revokeDevice', deviceId)
  renameDevice = (deviceId: string, name: string) =>
    this.call<never>('renameDevice', deviceId, name)
  signOut = () => this.call<void>('signOut')
  deleteAccount = () => this.call<void>('deleteAccount')
  listVaults = () => this.call<never>('listVaults')
  createVault = async (request: Parameters<Backend['createVault']>[0]) => {
    const vault = await this.call<never>('createVault', request)
    // The picked folder is a vault's now; nothing to discard.
    if (request.local_path === this.pickedHandleId) this.pickedHandleId = null
    return vault
  }
  downloadVault = async (request: Parameters<Backend['downloadVault']>[0]) => {
    const vault = await this.call<never>('downloadVault', request)
    if (request.local_path === this.pickedHandleId) this.pickedHandleId = null
    return vault
  }
  renameVault = (vaultId: string, name: string) => this.call<void>('renameVault', vaultId, name)
  removeVault = (vaultId: string) => this.call<void>('removeVault', vaultId)
  deleteRemoteVault = (vaultId: string) => this.call<void>('deleteRemoteVault', vaultId)
  syncVault = (vaultId: string) => this.call<never>('syncVault', vaultId)
  resolveConflict = (vaultId: string, resolutions: Parameters<Backend['resolveConflict']>[1]) =>
    this.call<never>('resolveConflict', vaultId, resolutions)
  getConflictPreview = (vaultId: string, path: string) =>
    this.call<never>('getConflictPreview', vaultId, path)
  listActivity = (vaultId: string | null, limit: number) =>
    this.call<never>('listActivity', vaultId, limit)
  setVisibility = (hidden: boolean) => this.call<void>('setVisibility', hidden)

  // A directory picker is a user gesture on the page; the handle goes to
  // IndexedDB under a fresh id that `addVault` receives as `local_path`.
  async pickFolder() {
    const handle = await window.showDirectoryPicker({ mode: 'readwrite' })
    // Picking again replaces the earlier pick rather than leaking it.
    await this.discardPickedFolder()
    const id = crypto.randomUUID()
    await put('handles', id, handle)
    this.pickedHandleId = id
    // Persistent storage keeps the handles and the sync bookkeeping from
    // being evicted under storage pressure; the browser may ask the user.
    void navigator.storage?.persist?.().catch(() => undefined)
    return { id, name: handle.name }
  }

  async discardPickedFolder() {
    const id = this.pickedHandleId
    if (!id) return
    this.pickedHandleId = null
    await del('handles', id)
  }

  // Chrome forgets the grant per session: ask again from a click.
  async requestFolderAccess(vaultId: string) {
    const vault = await get<StoredVault>('vaults', vaultId)
    const handle = vault && (await get<FileSystemDirectoryHandle>('handles', vault.handle_id))
    if (!handle)
      throw { kind: 'other', message: 'Folder not found. Remove the vault and download it again.' }
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
