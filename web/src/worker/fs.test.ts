import { describe, expect, it } from 'vitest'
import { asHandle, fakeRoot, type FakeDirectoryHandle, type FakeFileHandle } from './fake-fs'
import { exists, readFile, removeFile, scan, statOf, writeFile } from './fs'
import type { Core } from './wasm'

// The folder walk and the file operations over an in-memory directory: the
// same contract `fs.ts` gives the sync engine on a real File System Access
// handle.

type Ignore = InstanceType<Core['Ignore']>

function ignoring(...rules: ((path: string) => boolean)[]): Ignore {
  return { isIgnored: (path: string) => rules.some((rule) => rule(path)) } as unknown as Ignore
}

function dir(root: FakeDirectoryHandle, path: string): FakeDirectoryHandle | undefined {
  let current: FakeDirectoryHandle | undefined = root
  for (const part of path.split('/')) {
    const next: FakeDirectoryHandle | FakeFileHandle | undefined = current?.children.get(part)
    current = next?.kind === 'directory' ? next : undefined
  }
  return current
}

describe('scan', () => {
  it('lists files sorted by path, with size and mtime', async () => {
    const root = fakeRoot({ 'b.md': 'bb', 'a/x.md': 'x', 'a/b/y.md': 'yyy' }, { 'a/x.md': 1500 })
    const files = await scan(asHandle(root), ignoring())
    expect(files).toEqual([
      { path: 'a/b/y.md', size: 3, mtimeMs: 0 },
      { path: 'a/x.md', size: 1, mtimeMs: 1500 },
      { path: 'b.md', size: 2, mtimeMs: 0 },
    ])
  })

  it('skips ignored files and never walks into an ignored directory', async () => {
    const root = fakeRoot({
      'keep.md': '1',
      'skip/inner.md': '2',
      'skip/deep/x.md': '3',
      'c.tmp': '4',
    })
    const files = await scan(
      asHandle(root),
      ignoring(
        (path) => path === 'skip',
        (path) => path.endsWith('.tmp'),
      ),
    )
    expect(files.map((file) => file.path)).toEqual(['keep.md'])
    expect(dir(root, 'skip')?.visits).toBe(0)
  })
})

describe('statOf', () => {
  it('splits milliseconds into whole seconds and nanoseconds', () => {
    expect(statOf({ size: 7, mtimeMs: 1234567.891 })).toEqual({
      mtime_secs: 1234,
      mtime_nanos: 567891000,
      size: 7,
    })
  })
})

describe('read, write, exists', () => {
  it('writes through missing parents and reads the bytes back', async () => {
    const root = fakeRoot({})
    await writeFile(asHandle(root), 'notes/2026/today.md', new TextEncoder().encode('hello'))
    expect(new TextDecoder().decode(await readFile(asHandle(root), 'notes/2026/today.md'))).toBe(
      'hello',
    )
    expect(await exists(asHandle(root), 'notes/2026/today.md')).toBe(true)
    expect(await exists(asHandle(root), 'notes/2026/other.md')).toBe(false)
    expect(await exists(asHandle(root), 'nowhere/x.md')).toBe(false)
  })

  it('reports a missing file as NotFoundError', async () => {
    const root = fakeRoot({ 'a.md': 'a' })
    await expect(readFile(asHandle(root), 'b/c.md')).rejects.toMatchObject({
      name: 'NotFoundError',
    })
  })
})

describe('removeFile', () => {
  it('removes the file and only the parents it left empty', async () => {
    const root = fakeRoot({ 'a/b/c.md': '1', 'a/d.md': '2', 'top.md': '3' })
    await removeFile(asHandle(root), 'a/b/c.md')
    expect(root.snapshot()).toEqual({ 'a/d.md': '2', 'top.md': '3' })
    expect(dir(root, 'a/b')).toBeUndefined()
    await removeFile(asHandle(root), 'a/d.md')
    expect(root.snapshot()).toEqual({ 'top.md': '3' })
    expect(dir(root, 'a')).toBeUndefined()
  })

  it('is a no-op for a file that is already gone', async () => {
    const root = fakeRoot({ 'a.md': '1' })
    await expect(removeFile(asHandle(root), 'gone/x.md')).resolves.toBeUndefined()
    await expect(removeFile(asHandle(root), 'b.md')).resolves.toBeUndefined()
    expect(root.snapshot()).toEqual({ 'a.md': '1' })
  })
})
