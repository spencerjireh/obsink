import { describe, expect, it, vi } from 'vitest'

// `sync.ts` reaches `session.ts`, which reads the worker's origin at import.
vi.stubGlobal('self', { location: { origin: 'http://localhost' }, navigator: {} })
const { revisionOf } = await import('./sync')

describe('revisionOf', () => {
  it('reads the manifest revision from the ETag as core does', () => {
    expect(revisionOf('"12"')).toBe(12)
    expect(revisionOf('7')).toBe(7)
    expect(revisionOf(' "3" ')).toBe(3)
    expect(revisionOf('W/"x"')).toBeNull()
    expect(revisionOf(null)).toBeNull()
    expect(revisionOf(undefined)).toBeNull()
  })
})
