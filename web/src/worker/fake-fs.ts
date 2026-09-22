// An in-memory File System Access directory for tests: the subset of
// `FileSystemDirectoryHandle` / `FileSystemFileHandle` that `fs.ts` uses.

function concat(chunks: Uint8Array[]): Uint8Array {
  const total = chunks.reduce((sum, chunk) => sum + chunk.byteLength, 0)
  const out = new Uint8Array(total)
  let offset = 0
  for (const chunk of chunks) {
    out.set(chunk, offset)
    offset += chunk.byteLength
  }
  return out
}

function bytesOf(data: BufferSource | string): Uint8Array {
  if (typeof data === 'string') return new TextEncoder().encode(data)
  if (data instanceof ArrayBuffer) return new Uint8Array(data)
  const view = data as ArrayBufferView
  return new Uint8Array(view.buffer, view.byteOffset, view.byteLength)
}

export class FakeFileHandle {
  readonly kind = 'file' as const

  constructor(
    public name: string,
    public bytes: Uint8Array = new Uint8Array(),
    public lastModified = 0,
  ) {}

  async getFile() {
    const bytes = this.bytes
    return {
      name: this.name,
      size: bytes.byteLength,
      lastModified: this.lastModified,
      arrayBuffer: async () => bytes.slice().buffer,
      text: async () => new TextDecoder().decode(bytes),
    } as unknown as File
  }

  async createWritable() {
    const chunks: Uint8Array[] = []
    return {
      write: async (data: BufferSource | string) => {
        chunks.push(bytesOf(data))
      },
      close: async () => {
        this.bytes = concat(chunks)
        this.lastModified = Date.now()
      },
      abort: async () => undefined,
    } as unknown as FileSystemWritableFileStream
  }
}

export class FakeDirectoryHandle {
  readonly kind = 'directory' as const
  readonly children = new Map<string, FakeDirectoryHandle | FakeFileHandle>()
  // How many times the directory was walked (to show ignored ones are pruned).
  visits = 0

  constructor(public name: string) {}

  async *entries() {
    this.visits += 1
    for (const entry of this.children) yield entry as unknown as [string, FileSystemHandle]
  }

  async *keys() {
    for (const name of this.children.keys()) yield name
  }

  async getDirectoryHandle(name: string, options?: { create?: boolean }) {
    const existing = this.children.get(name)
    if (existing?.kind === 'directory') return existing
    if (existing) throw new DOMException(`${name} is a file`, 'TypeMismatchError')
    if (!options?.create) throw new DOMException(`${name} not found`, 'NotFoundError')
    const created = new FakeDirectoryHandle(name)
    this.children.set(name, created)
    return created
  }

  async getFileHandle(name: string, options?: { create?: boolean }) {
    const existing = this.children.get(name)
    if (existing?.kind === 'file') return existing
    if (existing) throw new DOMException(`${name} is a directory`, 'TypeMismatchError')
    if (!options?.create) throw new DOMException(`${name} not found`, 'NotFoundError')
    const created = new FakeFileHandle(name)
    this.children.set(name, created)
    return created
  }

  async removeEntry(name: string) {
    if (!this.children.delete(name)) throw new DOMException(`${name} not found`, 'NotFoundError')
  }

  // The tree as `path -> text`, for assertions.
  snapshot(prefix = ''): Record<string, string> {
    const out: Record<string, string> = {}
    for (const [name, child] of this.children) {
      const path = prefix ? `${prefix}/${name}` : name
      if (child.kind === 'file') out[path] = new TextDecoder().decode(child.bytes)
      else Object.assign(out, child.snapshot(path))
    }
    return out
  }
}

// A root populated from `path -> text` (with optional mtimes in ms).
export function fakeRoot(
  files: Record<string, string>,
  mtimes: Record<string, number> = {},
): FakeDirectoryHandle {
  const root = new FakeDirectoryHandle('vault')
  for (const [path, text] of Object.entries(files)) {
    const parts = path.split('/')
    const name = parts.pop() as string
    let dir = root
    for (const part of parts) {
      const next = dir.children.get(part)
      if (next?.kind === 'directory') dir = next
      else {
        const created = new FakeDirectoryHandle(part)
        dir.children.set(part, created)
        dir = created
      }
    }
    dir.children.set(
      name,
      new FakeFileHandle(name, new TextEncoder().encode(text), mtimes[path] ?? 0),
    )
  }
  return root
}

export function asHandle(root: FakeDirectoryHandle): FileSystemDirectoryHandle {
  return root as unknown as FileSystemDirectoryHandle
}
