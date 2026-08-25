import { beforeEach, describe, expect, it } from 'vitest'

import worker, { internal, type Env } from '../src/index'
import { overrides } from '../src/auth'
import { FakeKVNamespace, FakeR2Bucket, createContext, createRequest } from './index.test'

function createEnv(extra: Partial<Env> = {}): Env {
  return {
    API_KEY: 'secret',
    META: new FakeKVNamespace() as unknown as KVNamespace,
    FILES: new FakeR2Bucket() as unknown as R2Bucket,
    MAX_BATCH_INLINE_BYTES: String(50 * 1024 * 1024),
    AUTH_DEV_RETURN_CODE: '1',
    APPLE_CLIENT_IDS: 'com.obsink.ios',
    ...extra,
  }
}

const ctx = createContext()

async function post(env: Env, path: string, body: unknown, token?: string): Promise<Response> {
  return worker.fetch(
    createRequest(`https://example.com${path}`, {
      method: 'POST',
      headers: {
        'Content-Type': 'application/json',
        ...(token ? { Authorization: `Bearer ${token}` } : {}),
      },
      body: JSON.stringify(body),
    }),
    env,
    ctx,
  )
}

async function call(env: Env, method: string, path: string, token?: string): Promise<Response> {
  return worker.fetch(
    createRequest(`https://example.com${path}`, {
      method,
      headers: token ? { Authorization: `Bearer ${token}` } : {},
    }),
    env,
    ctx,
  )
}

/** Sign in with email + code and return the session token. */
async function signIn(env: Env, email: string, device = 'test'): Promise<string> {
  const start = await post(env, '/auth/email/start', { email })
  expect(start.status).toBe(200)
  const { code } = await start.json<{ code: string }>()
  const verify = await post(env, '/auth/email/verify', { email, code, device_name: device })
  expect(verify.status).toBe(200)
  return (await verify.json<{ token: string }>()).token
}

async function createVault(env: Env, token: string, name: string): Promise<{ status: number; id?: string }> {
  const response = await post(env, '/vaults', { name }, token)
  if (response.status !== 201) {
    return { status: response.status }
  }
  return { status: 201, id: (await response.json<{ vault: { id: string } }>()).vault.id }
}

// --- Apple identity-token fixture -------------------------------------------

function base64Url(bytes: Uint8Array | string): string {
  const binary =
    typeof bytes === 'string' ? bytes : Array.from(bytes, (byte) => String.fromCharCode(byte)).join('')
  return btoa(binary).replace(/\+/g, '-').replace(/\//g, '_').replace(/=+$/, '')
}

let appleKey: CryptoKeyPair
const APPLE_KID = 'test-kid'

async function appleToken(claims: Record<string, unknown>, keyOverride?: CryptoKey): Promise<string> {
  const header = base64Url(JSON.stringify({ alg: 'RS256', kid: APPLE_KID }))
  const payload = base64Url(
    JSON.stringify({
      iss: 'https://appleid.apple.com',
      aud: 'com.obsink.ios',
      exp: Math.floor(Date.now() / 1000) + 600,
      sub: '001234.abcdef',
      ...claims,
    }),
  )
  const signature = await crypto.subtle.sign(
    'RSASSA-PKCS1-v1_5',
    keyOverride ?? appleKey.privateKey,
    new TextEncoder().encode(`${header}.${payload}`),
  )
  return `${header}.${payload}.${base64Url(new Uint8Array(signature))}`
}

beforeEach(async () => {
  appleKey = await crypto.subtle.generateKey(
    { name: 'RSASSA-PKCS1-v1_5', modulusLength: 2048, publicExponent: new Uint8Array([1, 0, 1]), hash: 'SHA-256' },
    true,
    ['sign', 'verify'],
  )
  const jwk = await crypto.subtle.exportKey('jwk', appleKey.publicKey)
  overrides.fetchJwks = async () => [{ ...jwk, kid: APPLE_KID }]
})

describe('capabilities', () => {
  it('advertises configured sign-in methods without auth', async () => {
    const response = await call(createEnv({ RESEND_API_KEY: undefined, AUTH_DEV_RETURN_CODE: undefined }), 'GET', '/')
    expect(response.status).toBe(200)
    expect(await response.json()).toEqual({
      service: 'obsink',
      auth: { email: false, apple: true, api_key: true },
    })
  })
})

describe('email sign-in', () => {
  it('issues a session for a valid code and rejects a wrong one', async () => {
    const env = createEnv()
    const start = await post(env, '/auth/email/start', { email: 'Person@Example.com ' })
    expect(start.status).toBe(200)
    const { code, sent } = await start.json<{ code: string; sent: boolean }>()
    expect(sent).toBe(false) // no RESEND_API_KEY in tests
    expect(code).toMatch(/^\d{6}$/)

    const wrong = await post(env, '/auth/email/verify', { email: 'person@example.com', code: '000000' })
    expect(wrong.status).toBe(code === '000000' ? 200 : 401)

    const verify = await post(env, '/auth/email/verify', { email: 'person@example.com', code, device_name: 'iPhone' })
    expect(verify.status).toBe(200)
    const session = await verify.json<{ token: string; user: { email: string } }>()
    expect(session.token).toMatch(/^os_/)
    expect(session.user.email).toBe('person@example.com')

    // Code is single-use.
    const reuse = await post(env, '/auth/email/verify', { email: 'person@example.com', code })
    expect(reuse.status).toBe(401)

    const meResponse = await call(env, 'GET', '/auth/me', session.token)
    const me = await meResponse.json<any>()
    expect(me.kind).toBe('user')
    expect(me.user.email).toBe('person@example.com')
    expect(me.sessions).toHaveLength(1)
    expect(me.sessions[0].deviceName).toBe('iPhone')
    expect(me.sessions[0].current).toBe(true)
  })

  it('rate-limits code requests and refuses when email is not configured', async () => {
    const env = createEnv()
    expect((await post(env, '/auth/email/start', { email: 'a@b.co' })).status).toBe(200)
    expect((await post(env, '/auth/email/start', { email: 'a@b.co' })).status).toBe(429)
    expect((await post(env, '/auth/email/start', { email: 'not-an-email' })).status).toBe(400)

    const prod = createEnv({ AUTH_DEV_RETURN_CODE: undefined })
    expect((await post(prod, '/auth/email/start', { email: 'a@b.co' })).status).toBe(503)
  })

  it('locks the code after five wrong attempts', async () => {
    const env = createEnv()
    const start = await post(env, '/auth/email/start', { email: 'a@b.co' })
    const { code } = await start.json<{ code: string }>()
    const wrongCode = code === '111111' ? '222222' : '111111'
    for (let i = 0; i < 5; i += 1) {
      expect((await post(env, '/auth/email/verify', { email: 'a@b.co', code: wrongCode })).status).toBe(401)
    }
    // Even the right code is refused now.
    expect((await post(env, '/auth/email/verify', { email: 'a@b.co', code })).status).toBe(401)
  })

  it('returns the same account on repeat sign-in', async () => {
    const env = createEnv()
    const first = await signIn(env, 'a@b.co', 'one')
    ;(env.META as unknown as FakeKVNamespace).delete('otp_rl:' + (await sha256('a@b.co')))
    const second = await signIn(env, 'a@b.co', 'two')
    const me1 = await (await call(env, 'GET', '/auth/me', first)).json<any>()
    const me2 = await (await call(env, 'GET', '/auth/me', second)).json<any>()
    expect(me1.user.id).toBe(me2.user.id)
    expect(me2.sessions.map((s: any) => s.deviceName).sort()).toEqual(['one', 'two'])
  })
})

describe('Sign in with Apple', () => {
  it('accepts a valid identity token and links by email', async () => {
    const env = createEnv()
    const emailToken = await signIn(env, 'same@person.dev')
    const emailMe = await (await call(env, 'GET', '/auth/me', emailToken)).json<any>()

    const response = await post(env, '/auth/apple', {
      identity_token: await appleToken({ email: 'same@person.dev' }),
      device_name: 'iPhone',
    })
    expect(response.status).toBe(200)
    const session = await response.json<{ token: string; user: { id: string } }>()
    expect(session.user.id).toBe(emailMe.user.id)

    // Second Apple sign-in (Apple omits email after the first) resolves by sub.
    const again = await post(env, '/auth/apple', { identity_token: await appleToken({}) })
    expect(again.status).toBe(200)
    expect((await again.json<{ user: { id: string } }>()).user.id).toBe(emailMe.user.id)
  })

  it('rejects bad signature, wrong audience, wrong issuer, and expiry', async () => {
    const env = createEnv()
    const other = await crypto.subtle.generateKey(
      { name: 'RSASSA-PKCS1-v1_5', modulusLength: 2048, publicExponent: new Uint8Array([1, 0, 1]), hash: 'SHA-256' },
      true,
      ['sign', 'verify'],
    )
    const cases = [
      await appleToken({}, other.privateKey),
      await appleToken({ aud: 'com.someone.else' }),
      await appleToken({ iss: 'https://evil.example' }),
      await appleToken({ exp: Math.floor(Date.now() / 1000) - 5 }),
      'not.a.jwt',
    ]
    for (const identity_token of cases) {
      expect((await post(env, '/auth/apple', { identity_token })).status).toBe(401)
    }
    expect((await post(createEnv({ APPLE_CLIENT_IDS: undefined }), '/auth/apple', { identity_token: await appleToken({}) })).status).toBe(503)
  })
})

describe('tenant scoping', () => {
  it('isolates vault lists between accounts and the operator', async () => {
    const env = createEnv()
    const alice = await signIn(env, 'alice@x.io')
    const bob = await signIn(env, 'bob@x.io')

    const aliceVault = await createVault(env, alice, 'alice-notes')
    expect(aliceVault.status).toBe(201)
    const operatorVault = await createVault(env, 'secret', 'operator-notes')
    expect(operatorVault.status).toBe(201)

    const bobList = await (await call(env, 'GET', '/vaults', bob)).json<any[]>()
    expect(bobList).toEqual([])
    const aliceList = await (await call(env, 'GET', '/vaults', alice)).json<any[]>()
    expect(aliceList.map((v) => v.name)).toEqual(['alice-notes'])
    const operatorList = await (await call(env, 'GET', '/vaults', 'secret')).json<any[]>()
    expect(operatorList.map((v) => v.name)).toEqual(['operator-notes'])

    // Bob cannot read, write, or delete Alice's vault, even knowing its id.
    expect((await call(env, 'GET', `/vaults/${aliceVault.id}/manifest`, bob)).status).toBe(404)
    const put = await worker.fetch(
      createRequest(`https://example.com/vaults/${aliceVault.id}/files/tok`, {
        method: 'PUT',
        headers: { Authorization: `Bearer ${bob}`, 'X-Content-Hash': 'h' },
        body: 'x',
      }),
      env,
      ctx,
    )
    expect(put.status).toBe(404)
    expect((await call(env, 'DELETE', `/vaults/${aliceVault.id}`, bob)).status).toBe(404)
    // The operator key is not a superuser over account vaults either.
    expect((await call(env, 'GET', `/vaults/${aliceVault.id}/manifest`, 'secret')).status).toBe(404)
    // Alice can.
    expect((await call(env, 'GET', `/vaults/${aliceVault.id}/manifest`, alice)).status).toBe(200)
  })

  it('enforces the per-account vault limit and per-vault byte budget', async () => {
    const env = createEnv({ MAX_VAULTS_PER_USER: '2', MAX_VAULT_BYTES: '10' })
    const alice = await signIn(env, 'alice@x.io')
    expect((await createVault(env, alice, 'one')).status).toBe(201)
    const { id } = await createVault(env, alice, 'two')
    expect((await createVault(env, alice, 'three')).status).toBe(403)

    const put = (body: string, path: string) =>
      worker.fetch(
        createRequest(`https://example.com/vaults/${id}/files/${path}`, {
          method: 'PUT',
          headers: { Authorization: `Bearer ${alice}`, 'X-Content-Hash': 'h', 'X-Enc-Path': 'e' },
          body,
        }),
        env,
        ctx,
      )
    expect((await put('123456', 'a')).status).toBe(200)
    expect((await put('12345', 'b')).status).toBe(413) // 6 + 5 > 10
    expect((await put('1234', 'b')).status).toBe(200) // 6 + 4 = 10
  })

  it('deletes a vault and all its blobs', async () => {
    const env = createEnv()
    const alice = await signIn(env, 'alice@x.io')
    const { id } = await createVault(env, alice, 'gone')
    await worker.fetch(
      createRequest(`https://example.com/vaults/${id}/files/tok`, {
        method: 'PUT',
        headers: { Authorization: `Bearer ${alice}`, 'X-Content-Hash': 'h', 'X-Enc-Path': 'e' },
        body: 'hello',
      }),
      env,
      ctx,
    )
    expect((env.FILES as unknown as FakeR2Bucket).keys()).toEqual([`${id}/tok`])
    expect((await call(env, 'DELETE', `/vaults/${id}`, alice)).status).toBe(204)
    expect((env.FILES as unknown as FakeR2Bucket).keys()).toEqual([])
    expect(await (await call(env, 'GET', '/vaults', alice)).json()).toEqual([])
  })
})

describe('sessions and account deletion', () => {
  it('revokes sessions and deletes the account with its vaults', async () => {
    const env = createEnv()
    const phone = await signIn(env, 'alice@x.io', 'phone')
    ;(env.META as unknown as FakeKVNamespace).delete('otp_rl:' + (await sha256('alice@x.io')))
    const laptop = await signIn(env, 'alice@x.io', 'laptop')

    // Sign out the phone from the laptop.
    const me = await (await call(env, 'GET', '/auth/me', laptop)).json<any>()
    const phoneSession = me.sessions.find((s: any) => s.deviceName === 'phone')
    expect((await call(env, 'DELETE', `/auth/sessions/${phoneSession.id}`, laptop)).status).toBe(204)
    expect((await call(env, 'GET', '/auth/me', phone)).status).toBe(401)

    const { id } = await createVault(env, laptop, 'v')
    expect((await call(env, 'DELETE', '/auth/account', laptop)).status).toBe(204)
    expect((await call(env, 'GET', '/auth/me', laptop)).status).toBe(401)
    expect(await internal.readManifest(env, id!)).toEqual({})
    const keys = (env.META as unknown as FakeKVNamespace).keys()
    expect(keys.filter((k) => k.startsWith('user') || k.startsWith('session') || k.startsWith('vault:'))).toEqual([])
  })

  it('the operator bearer has no session to revoke', async () => {
    const env = createEnv()
    expect((await call(env, 'DELETE', '/auth/session', 'secret')).status).toBe(400)
    const me = await (await call(env, 'GET', '/auth/me', 'secret')).json<any>()
    expect(me.kind).toBe('operator')
  })
})

async function sha256(value: string): Promise<string> {
  const digest = await crypto.subtle.digest('SHA-256', new TextEncoder().encode(value))
  return Array.from(new Uint8Array(digest), (b) => b.toString(16).padStart(2, '0')).join('')
}
