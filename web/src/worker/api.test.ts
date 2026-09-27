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

// Spec §4.1, the four device-approval routes: what each call sends and how
// the answers come back (the crypto around them lives in core-wasm).
describe('Api device approval', () => {
  it('registers a request with the public key and returns its window', async () => {
    fetchMock.mockResolvedValue(json(201, { requested: 100, expires: 700 }))
    const registered = await api.registerApproval('t', 'PUBKEY==')
    const { url, init } = requestOf(0)
    expect(url).toBe('http://api.test/auth/approval')
    expect(init.method).toBe('PUT')
    expect((init.headers as Headers).get('Authorization')).toBe('Bearer t')
    expect((init.headers as Headers).get('Content-Type')).toBe('application/json')
    expect(JSON.parse(init.body as string)).toEqual({ public_key: 'PUBKEY==' })
    expect(registered).toEqual({ requested: 100, expires: 700 })
  })

  it('polls the request and unwraps the approval field, null when none is live', async () => {
    fetchMock.mockResolvedValueOnce(
      json(200, {
        approval: {
          public_key: 'PUBKEY==',
          requested: 100,
          expires: 700,
          wrapped: 'BLOB==',
          key_id: 'key_1',
          approved_by: 'dev_a',
          approved: 200,
        },
      }),
    )
    const status = await api.approvalStatus('t')
    const { url, init } = requestOf(0)
    expect(url).toBe('http://api.test/auth/approval')
    expect(init.method).toBe('GET')
    expect(status).toEqual({
      public_key: 'PUBKEY==',
      requested: 100,
      expires: 700,
      wrapped: 'BLOB==',
      key_id: 'key_1',
      approved_by: 'dev_a',
      approved: 200,
    })

    fetchMock.mockResolvedValueOnce(json(200, { approval: null }))
    expect(await api.approvalStatus('t')).toBeNull()
  })

  it('withdraws the request with a bodiless DELETE', async () => {
    fetchMock.mockResolvedValue(new Response(null, { status: 204 }))
    await api.clearApproval('t')
    const { url, init } = requestOf(0)
    expect(url).toBe('http://api.test/auth/approval')
    expect(init.method).toBe('DELETE')
    expect(init.body).toBeUndefined()
    expect(fetchMock).toHaveBeenCalledTimes(1)
  })

  it('posts the wrapped key and the verifier to the pending device', async () => {
    fetchMock.mockResolvedValue(new Response(null, { status: 204 }))
    await api.approveDevice('t', 'dev/b', 'WRAPPED==', 'VERIFIER==')
    const { url, init } = requestOf(0)
    expect(url).toBe('http://api.test/auth/devices/dev%2Fb/approval')
    expect(init.method).toBe('POST')
    expect(JSON.parse(init.body as string)).toEqual({
      wrapped: 'WRAPPED==',
      verifier: 'VERIFIER==',
    })
  })

  it('reports an expired request (404) and an earlier approval (409) as server errors', async () => {
    fetchMock.mockResolvedValueOnce(json(404, { error: 'no pending request for this device' }))
    await expect(api.approveDevice('t', 'dev_b', 'W', 'V')).rejects.toMatchObject({
      kind: 'server',
      status: 404,
      message: 'no pending request for this device',
    })
    fetchMock.mockResolvedValueOnce(json(409, { error: 'already approved' }))
    await expect(api.approveDevice('t', 'dev_b', 'W', 'V')).rejects.toMatchObject({
      kind: 'server',
      status: 409,
      message: 'already approved',
    })
    // A POST is never replayed: one fetch per call.
    expect(fetchMock).toHaveBeenCalledTimes(2)
  })
})
