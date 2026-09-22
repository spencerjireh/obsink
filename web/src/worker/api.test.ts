import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest'
import { Api } from './api'

// The wire layer against a stubbed fetch: status mapping, the retry policy
// (three attempts for a request that got no answer, and only when repeating
// it is safe), and the headers every call carries.

const api = new Api('http://api.test')
let fetchMock: ReturnType<typeof vi.fn>

function json(status: number, body: unknown): Response {
  return new Response(JSON.stringify(body), {
    status,
    headers: { 'Content-Type': 'application/json' },
  })
}

function requestOf(call: number): { url: string; init: RequestInit } {
  const [url, init] = fetchMock.mock.calls[call] as [string, RequestInit]
  return { url, init }
}

beforeEach(() => {
  fetchMock = vi.fn()
  vi.stubGlobal('fetch', fetchMock)
  vi.useFakeTimers()
})

afterEach(() => {
  vi.unstubAllGlobals()
  vi.useRealTimers()
})

describe('Api', () => {
  it('sends the bearer and the method once, to the base URL', async () => {
    fetchMock.mockResolvedValue(json(200, { kind: 'account', user: null }))
    await api.me('token-1')
    const { url, init } = requestOf(0)
    expect(url).toBe('http://api.test/auth/me')
    expect(init.method).toBe('GET')
    expect((init.headers as Headers).get('Authorization')).toBe('Bearer token-1')
    expect(fetchMock).toHaveBeenCalledTimes(1)
  })

  it('turns a 401 into an unauthorized error with the shared message', async () => {
    fetchMock.mockResolvedValue(json(401, { error: 'bad token' }))
    await expect(api.me('t')).rejects.toMatchObject({
      kind: 'unauthorized',
      status: 401,
      message: 'Session expired. Sign in again.',
    })
    expect(fetchMock).toHaveBeenCalledTimes(1)
  })

  it('does not retry a server error and keeps its message', async () => {
    fetchMock.mockResolvedValue(json(500, { error: 'database down' }))
    await expect(api.listVaults('t')).rejects.toMatchObject({
      kind: 'server',
      status: 500,
      message: 'database down',
    })
    expect(fetchMock).toHaveBeenCalledTimes(1)
  })

  it('retries a GET that never got an answer, three times, then reports the network', async () => {
    fetchMock.mockRejectedValue(new TypeError('fetch failed'))
    const rejection = expect(api.listVaults('t')).rejects.toMatchObject({ kind: 'network' })
    await vi.runAllTimersAsync()
    await rejection
    expect(fetchMock).toHaveBeenCalledTimes(3)
  })

  it('never repeats a POST the server may already have applied', async () => {
    fetchMock.mockRejectedValue(new TypeError('fetch failed'))
    const rejection = expect(api.emailStart('a@b.test')).rejects.toMatchObject({
      kind: 'network',
    })
    await vi.runAllTimersAsync()
    await rejection
    expect(fetchMock).toHaveBeenCalledTimes(1)
  })

  it('returns a 304 the caller asked for and sends If-None-Match', async () => {
    fetchMock.mockResolvedValue(new Response(null, { status: 304 }))
    const result = await api.getManifest('t', 'vault_1', '"etag-1"')
    expect(result).toEqual({ status: 'not_modified' })
    expect((requestOf(0).init.headers as Headers).get('If-None-Match')).toBe('"etag-1"')
  })

  it('probes the server root for its capabilities as JSON', async () => {
    fetchMock.mockResolvedValue(json(200, { service: 'obsink', auth: { email: true } }))
    await api.capabilities()
    const { url, init } = requestOf(0)
    expect(url).toBe('http://api.test/')
    expect((init.headers as Headers).get('Accept')).toBe('application/json')
  })
})
