import type { Stat } from '../shared/db'
import type { Core } from './wasm'

// The vault folder through the File System Access API: the walk the native
// hasher does with walkdir, reads, writes and deletes. Paths are
// vault-relative with `/` separators, as everywhere in core.

export type ScannedFile = { path: string; size: number; mtimeMs: number }

// Every file under the root, ignore rules applied to files and to whole
// directories (a `dir/` rule prunes the walk like the native walker).
export async function scan(
  root: FileSystemDirectoryHandle,
  ignore: InstanceType<Core['Ignore']>,
): Promise<ScannedFile[]> {
  const found: ScannedFile[] = []
  async function walk(dir: FileSystemDirectoryHandle, prefix: string) {
    for await (const [name, handle] of dir.entries()) {
      const path = prefix ? `${prefix}/${name}` : name
      if (handle.kind === 'directory') {
        if (ignore.isIgnored(path)) continue
        await walk(handle, path)
      } else {
        if (ignore.isIgnored(path)) continue
        const file = await handle.getFile()
        found.push({ path, size: file.size, mtimeMs: file.lastModified })
      }
    }
  }
  await walk(root, '')
  found.sort((a, b) => (a.path < b.path ? -1 : a.path > b.path ? 1 : 0))
  return found
}

// The hash-cache key: the browser has millisecond mtimes, so nanos are
// always a multiple of a million; the shape matches core's `Stat`.
export function statOf(file: { size: number; mtimeMs: number }): Stat {
  return {
    mtime_secs: Math.floor(file.mtimeMs / 1000),
    mtime_nanos: Math.round((file.mtimeMs % 1000) * 1_000_000),
    size: file.size,
  }
}

function split(path: string): { dirs: string[]; name: string } {
  const parts = path.split('/').filter((part) => part.length > 0)
  const name = parts.pop()
  if (!name) throw new Error(`path has no file name: ${path}`)
  return { dirs: parts, name }
}

async function directory(
  root: FileSystemDirectoryHandle,
  dirs: string[],
  create: boolean,
): Promise<FileSystemDirectoryHandle | null> {
  let current = root
  for (const dir of dirs) {
    try {
      current = await current.getDirectoryHandle(dir, { create })
    } catch (error) {
      if (!create && (error as DOMException)?.name === 'NotFoundError') return null
      throw error
    }
  }
  return current
}

export async function readFile(root: FileSystemDirectoryHandle, path: string): Promise<Uint8Array> {
  const { dirs, name } = split(path)
  const dir = await directory(root, dirs, false)
  if (!dir) throw new DOMException(`${path} not found`, 'NotFoundError')
  const file = await (await dir.getFileHandle(name)).getFile()
  return new Uint8Array(await file.arrayBuffer())
}

export async function exists(root: FileSystemDirectoryHandle, path: string): Promise<boolean> {
  const { dirs, name } = split(path)
  const dir = await directory(root, dirs, false)
  if (!dir) return false
  try {
    await dir.getFileHandle(name)
    return true
  } catch (error) {
    if ((error as DOMException)?.name === 'NotFoundError') return false
    throw error
  }
}

// `createWritable` stages the bytes and swaps them in on `close`, which is
// the atomicity `write_atomic` gets from a temp file plus rename.
export async function writeFile(
  root: FileSystemDirectoryHandle,
  path: string,
  bytes: Uint8Array,
): Promise<void> {
  const { dirs, name } = split(path)
  const dir = (await directory(root, dirs, true)) as FileSystemDirectoryHandle
  const handle = await dir.getFileHandle(name, { create: true })
  const writable = await handle.createWritable()
  try {
    await writable.write(bytes as BufferSource)
  } catch (error) {
    await writable.abort()
    throw error
  }
  await writable.close()
}

// Remove the file, then every parent left empty up to (not including) the root.
export async function removeFile(root: FileSystemDirectoryHandle, path: string): Promise<void> {
  const { dirs, name } = split(path)
  const dir = await directory(root, dirs, false)
  if (!dir) return
  try {
    await dir.removeEntry(name)
  } catch (error) {
    if ((error as DOMException)?.name !== 'NotFoundError') throw error
  }
  for (let depth = dirs.length; depth > 0; depth--) {
    const parent = (await directory(
      root,
      dirs.slice(0, depth - 1),
      false,
    )) as FileSystemDirectoryHandle
    const current = await parent.getDirectoryHandle(dirs[depth - 1]).catch(() => null)
    if (!current) break
    let empty = true
    for await (const _ of current.keys()) {
      void _
      empty = false
      break
    }
    if (!empty) break
    await parent.removeEntry(dirs[depth - 1])
  }
}
