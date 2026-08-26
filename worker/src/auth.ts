/**
 * Accounts and sessions for the hosted ("ObSink Cloud") mode.
 *
 * Two kinds of principal can call the Worker:
 *   - operator: `Authorization: Bearer <API_KEY>` — the self-hosting credential.
 *     Owns the legacy `vaults` list. Unchanged behaviour for self-hosters.
 *   - user: `Authorization: Bearer os_<random>` — a session minted by
 *     `/auth/email/verify` or `/auth/apple`. Owns `vaults:<userId>`.
 *
 * Tokens are stored hashed (SHA-256) in KV; the plaintext exists only on the
 * client. Nothing here touches vault content: the server still never sees
 * plaintext notes (AGENTS.md rule 1), accounts only decide *which* encrypted
 * vaults a bearer may list and write.
 */

export interface AuthEnv {
  META: KVNamespace
  /** Operator bearer (self-hosting). Optional on a hosted deployment. */
  API_KEY?: string
  /** Resend API key for one-time-code email. Absent = email sign-in disabled. */
  RESEND_API_KEY?: string
  /** From address for one-time-code email, e.g. `ObSink <login@obsink.app>`. */
  MAIL_FROM?: string
  /** Comma-separated accepted `aud` values for Apple identity tokens (bundle IDs / service IDs). */
  APPLE_CLIENT_IDS?: string
  /** Dev/test only: "1" returns the one-time code in the /auth/email/start response. */
  AUTH_DEV_RETURN_CODE?: string
  MAX_VAULTS_PER_USER?: string
}

export type Principal =
  | { kind: 'operator'; tenant: 'default' }
  | { kind: 'user'; tenant: string; sessionId: string }

export const OPERATOR_TENANT = 'default'

export interface UserRecord {
  id: string
  email: string | null
  appleSub: string | null
  created: number
}

export interface SessionRecord {
  id: string
  userId: string
  deviceName: string
  created: number
  expires: number
}

interface SessionIndexEntry {
  id: string
  tokenHash: string
  deviceName: string
  created: number
}

interface OtpRecord {
  codeHash: string
  expires: number
  attempts: number
}

const OTP_TTL_SECS = 10 * 60
const OTP_RESEND_COOLDOWN_SECS = 60
const OTP_MAX_ATTEMPTS = 5
const SESSION_TTL_SECS = 180 * 24 * 60 * 60
const APPLE_ISSUER = 'https://appleid.apple.com'
const APPLE_JWKS_URL = 'https://appleid.apple.com/auth/keys'
const JWKS_CACHE_SECS = 60 * 60

export class AuthError extends Error {
  constructor(
    public readonly status: number,
    message: string,
  ) {
    super(message)
  }
}

// ---------------------------------------------------------------------------
// Principal resolution

export async function resolvePrincipal(request: Request, env: AuthEnv): Promise<Principal | null> {
  const auth = request.headers.get('Authorization') ?? ''
  if (!auth.startsWith('Bearer ')) {
    return null
  }
  const token = auth.slice('Bearer '.length).trim()
  if (!token) {
    return null
  }

  if (env.API_KEY && safeEqual(token, env.API_KEY)) {
    return { kind: 'operator', tenant: OPERATOR_TENANT }
  }

  if (!token.startsWith('os_')) {
    return null
  }

  const session = await env.META.get<SessionRecord>(sessionKey(await sha256Hex(token)), 'json')
  if (!session || session.expires <= nowSeconds()) {
    return null
  }
  return { kind: 'user', tenant: session.userId, sessionId: session.id }
}

// ---------------------------------------------------------------------------
// Email one-time code

export interface EmailStartResult {
  sent: boolean
  /** Only populated when AUTH_DEV_RETURN_CODE=1 (dev/test). */
  code?: string
}

export async function emailStart(env: AuthEnv, rawEmail: unknown): Promise<EmailStartResult> {
  const email = normalizeEmail(rawEmail)
  const emailHash = await sha256Hex(email)

  if (await env.META.get(otpCooldownKey(emailHash))) {
    throw new AuthError(429, 'a code was sent recently; wait a minute and try again')
  }

  const devReturn = env.AUTH_DEV_RETURN_CODE === '1'
  if (!devReturn && !env.RESEND_API_KEY) {
    throw new AuthError(503, 'email sign-in is not configured on this server')
  }

  const code = randomDigits(6)
  const record: OtpRecord = {
    codeHash: await sha256Hex(`${emailHash}:${code}`),
    expires: nowSeconds() + OTP_TTL_SECS,
    attempts: 0,
  }
  await env.META.put(otpKey(emailHash), JSON.stringify(record), { expirationTtl: OTP_TTL_SECS })
  await env.META.put(otpCooldownKey(emailHash), '1', { expirationTtl: OTP_RESEND_COOLDOWN_SECS })

  if (env.RESEND_API_KEY) {
    try {
      await sendCodeEmail(env, email, code)
    } catch (error) {
      // A failed send must not burn the resend cooldown or leave a code behind.
      await env.META.delete(otpKey(emailHash))
      await env.META.delete(otpCooldownKey(emailHash))
      throw error
    }
  }

  return devReturn ? { sent: Boolean(env.RESEND_API_KEY), code } : { sent: true }
}

export interface SessionResponse {
  token: string
  session: { id: string; expires: number }
  user: { id: string; email: string | null }
}

export async function emailVerify(
  env: AuthEnv,
  rawEmail: unknown,
  rawCode: unknown,
  deviceName: unknown,
): Promise<SessionResponse> {
  const email = normalizeEmail(rawEmail)
  const code = typeof rawCode === 'string' ? rawCode.replace(/\s+/g, '') : ''
  if (!/^\d{6}$/.test(code)) {
    throw new AuthError(400, 'code must be 6 digits')
  }

  const emailHash = await sha256Hex(email)
  const record = await env.META.get<OtpRecord>(otpKey(emailHash), 'json')
  if (!record || record.expires <= nowSeconds()) {
    throw new AuthError(401, 'code expired; request a new one')
  }
  if (record.attempts >= OTP_MAX_ATTEMPTS) {
    await env.META.delete(otpKey(emailHash))
    throw new AuthError(401, 'too many attempts; request a new code')
  }

  const expected = await sha256Hex(`${emailHash}:${code}`)
  if (!safeEqual(expected, record.codeHash)) {
    record.attempts += 1
    await env.META.put(otpKey(emailHash), JSON.stringify(record), {
      expirationTtl: Math.max(60, record.expires - nowSeconds()),
    })
    throw new AuthError(401, 'incorrect code')
  }
  await env.META.delete(otpKey(emailHash))

  const user = await findOrCreateUserByEmail(env, email, emailHash)
  return createSession(env, user, deviceName)
}

// ---------------------------------------------------------------------------
// Sign in with Apple

export interface AppleClaims {
  iss: string
  aud: string | string[]
  exp: number
  sub: string
  email?: string
  email_verified?: boolean | string
}

export async function appleSignIn(
  env: AuthEnv,
  identityToken: unknown,
  deviceName: unknown,
  /** Email from the first-run Apple credential (only delivered once by iOS). */
  hintedEmail: unknown,
): Promise<SessionResponse> {
  if (typeof identityToken !== 'string' || !identityToken) {
    throw new AuthError(400, 'identity_token is required')
  }
  const audiences = (env.APPLE_CLIENT_IDS ?? '')
    .split(',')
    .map((value) => value.trim())
    .filter(Boolean)
  if (audiences.length === 0) {
    throw new AuthError(503, 'Sign in with Apple is not configured on this server')
  }

  const claims = await verifyAppleIdentityToken(identityToken, audiences)
  const email = normalizeOptionalEmail(claims.email) ?? normalizeOptionalEmail(hintedEmail)

  const user = await findOrCreateUserByApple(env, claims.sub, email)
  return createSession(env, user, deviceName)
}

/** Test seam: replace the JWKS fetch (vitest cannot reach Apple). */
type AppleJwk = JsonWebKey & { kid?: string }

export const overrides: { fetchJwks: () => Promise<AppleJwk[]> } = {
  fetchJwks: async () => {
    const response = await fetch(APPLE_JWKS_URL)
    if (!response.ok) {
      throw new AuthError(502, 'could not fetch Apple signing keys')
    }
    const body = (await response.json()) as { keys: AppleJwk[] }
    return body.keys
  },
}

let jwksCache: { keys: AppleJwk[]; fetched: number } | null = null

async function appleKeys(forceRefresh = false): Promise<AppleJwk[]> {
  if (!forceRefresh && jwksCache && nowSeconds() - jwksCache.fetched < JWKS_CACHE_SECS) {
    return jwksCache.keys
  }
  const keys = await overrides.fetchJwks()
  jwksCache = { keys, fetched: nowSeconds() }
  return keys
}

export async function verifyAppleIdentityToken(token: string, audiences: string[]): Promise<AppleClaims> {
  const parts = token.split('.')
  if (parts.length !== 3) {
    throw new AuthError(401, 'malformed identity token')
  }
  let header: { kid?: string; alg?: string }
  let claims: AppleClaims
  try {
    header = JSON.parse(base64UrlDecodeToString(parts[0])) as { kid?: string; alg?: string }
    claims = JSON.parse(base64UrlDecodeToString(parts[1])) as AppleClaims
  } catch {
    throw new AuthError(401, 'malformed identity token')
  }
  if (header.alg !== 'RS256' || !header.kid) {
    throw new AuthError(401, 'unsupported identity token')
  }

  let jwk = (await appleKeys()).find((key) => key.kid === header.kid)
  if (!jwk) {
    // Apple rotates keys; refetch once before giving up.
    jwk = (await appleKeys(true)).find((key) => key.kid === header.kid)
  }
  if (!jwk) {
    throw new AuthError(401, 'unknown Apple signing key')
  }

  const key = await crypto.subtle.importKey(
    'jwk',
    jwk,
    { name: 'RSASSA-PKCS1-v1_5', hash: 'SHA-256' },
    false,
    ['verify'],
  )
  const signed = new TextEncoder().encode(`${parts[0]}.${parts[1]}`)
  const valid = await crypto.subtle.verify(
    'RSASSA-PKCS1-v1_5',
    key,
    base64UrlDecode(parts[2]),
    signed,
  )
  if (!valid) {
    throw new AuthError(401, 'identity token signature is invalid')
  }

  if (claims.iss !== APPLE_ISSUER) {
    throw new AuthError(401, 'identity token issuer mismatch')
  }
  const aud = Array.isArray(claims.aud) ? claims.aud : [claims.aud]
  if (!aud.some((value) => audiences.includes(value))) {
    throw new AuthError(401, 'identity token audience mismatch')
  }
  if (typeof claims.exp !== 'number' || claims.exp <= nowSeconds()) {
    throw new AuthError(401, 'identity token expired')
  }
  if (typeof claims.sub !== 'string' || !claims.sub) {
    throw new AuthError(401, 'identity token has no subject')
  }
  return claims
}

// ---------------------------------------------------------------------------
// Sessions and account

export interface MeResponse {
  kind: 'operator' | 'user'
  user: { id: string; email: string | null; created: number } | null
  sessions: Array<{ id: string; deviceName: string; created: number; current: boolean }>
}

export async function me(env: AuthEnv, principal: Principal): Promise<MeResponse> {
  if (principal.kind === 'operator') {
    return { kind: 'operator', user: null, sessions: [] }
  }
  const user = await env.META.get<UserRecord>(userKey(principal.tenant), 'json')
  const index = (await env.META.get<SessionIndexEntry[]>(userSessionsKey(principal.tenant), 'json')) ?? []
  return {
    kind: 'user',
    user: user ? { id: user.id, email: user.email, created: user.created } : null,
    sessions: index.map((entry) => ({
      id: entry.id,
      deviceName: entry.deviceName,
      created: entry.created,
      current: entry.id === principal.sessionId,
    })),
  }
}

/** Revoke one session (the current one when `sessionId` is omitted). */
export async function revokeSession(env: AuthEnv, principal: Principal, sessionId?: string): Promise<void> {
  if (principal.kind !== 'user') {
    throw new AuthError(400, 'operator bearer has no session')
  }
  const target = sessionId ?? principal.sessionId
  const index = (await env.META.get<SessionIndexEntry[]>(userSessionsKey(principal.tenant), 'json')) ?? []
  const entry = index.find((item) => item.id === target)
  if (!entry) {
    throw new AuthError(404, 'session not found')
  }
  await env.META.delete(sessionKey(entry.tokenHash))
  await env.META.put(
    userSessionsKey(principal.tenant),
    JSON.stringify(index.filter((item) => item.id !== target)),
  )
}

/**
 * Delete the account: every session, the email/Apple lookups, and the user
 * record. Vault data removal is the caller's job (index.ts owns storage) —
 * it runs `deleteVaultData` for each vault first, then calls this.
 */
export async function deleteAccount(env: AuthEnv, principal: Principal): Promise<void> {
  if (principal.kind !== 'user') {
    throw new AuthError(400, 'operator bearer has no account')
  }
  const userId = principal.tenant
  const index = (await env.META.get<SessionIndexEntry[]>(userSessionsKey(userId), 'json')) ?? []
  await Promise.all(index.map((entry) => env.META.delete(sessionKey(entry.tokenHash))))
  await env.META.delete(userSessionsKey(userId))

  const user = await env.META.get<UserRecord>(userKey(userId), 'json')
  if (user?.email) {
    await env.META.delete(userEmailKey(await sha256Hex(user.email)))
  }
  if (user?.appleSub) {
    await env.META.delete(userAppleKey(user.appleSub))
  }
  await env.META.delete(userKey(userId))
}

export function maxVaultsPerUser(env: AuthEnv): number {
  const value = Number(env.MAX_VAULTS_PER_USER ?? 10)
  return Number.isFinite(value) && value > 0 ? value : 10
}

// ---------------------------------------------------------------------------
// Internals

async function findOrCreateUserByEmail(env: AuthEnv, email: string, emailHash: string): Promise<UserRecord> {
  const existingId = await env.META.get(userEmailKey(emailHash))
  if (existingId) {
    const user = await env.META.get<UserRecord>(userKey(existingId), 'json')
    if (user) {
      return user
    }
  }
  const user: UserRecord = {
    id: `usr_${crypto.randomUUID()}`,
    email,
    appleSub: null,
    created: nowSeconds(),
  }
  await env.META.put(userKey(user.id), JSON.stringify(user))
  await env.META.put(userEmailKey(emailHash), user.id)
  return user
}

async function findOrCreateUserByApple(env: AuthEnv, sub: string, email: string | null): Promise<UserRecord> {
  const bySub = await env.META.get(userAppleKey(sub))
  if (bySub) {
    const user = await env.META.get<UserRecord>(userKey(bySub), 'json')
    if (user) {
      return user
    }
  }

  // Link to an existing email account (same person signing in a second way).
  if (email) {
    const emailHash = await sha256Hex(email)
    const byEmail = await env.META.get(userEmailKey(emailHash))
    if (byEmail) {
      const user = await env.META.get<UserRecord>(userKey(byEmail), 'json')
      if (user) {
        user.appleSub = sub
        await env.META.put(userKey(user.id), JSON.stringify(user))
        await env.META.put(userAppleKey(sub), user.id)
        return user
      }
    }
  }

  const user: UserRecord = {
    id: `usr_${crypto.randomUUID()}`,
    email,
    appleSub: sub,
    created: nowSeconds(),
  }
  await env.META.put(userKey(user.id), JSON.stringify(user))
  await env.META.put(userAppleKey(sub), user.id)
  if (email) {
    await env.META.put(userEmailKey(await sha256Hex(email)), user.id)
  }
  return user
}

async function createSession(env: AuthEnv, user: UserRecord, rawDeviceName: unknown): Promise<SessionResponse> {
  const deviceName =
    typeof rawDeviceName === 'string' && rawDeviceName.trim() ? rawDeviceName.trim().slice(0, 80) : 'device'
  const token = `os_${base64UrlEncode(crypto.getRandomValues(new Uint8Array(32)))}`
  const tokenHash = await sha256Hex(token)
  const now = nowSeconds()
  const session: SessionRecord = {
    id: `ses_${crypto.randomUUID()}`,
    userId: user.id,
    deviceName,
    created: now,
    expires: now + SESSION_TTL_SECS,
  }
  await env.META.put(sessionKey(tokenHash), JSON.stringify(session), { expirationTtl: SESSION_TTL_SECS })

  const index = (await env.META.get<SessionIndexEntry[]>(userSessionsKey(user.id), 'json')) ?? []
  index.push({ id: session.id, tokenHash, deviceName, created: now })
  await env.META.put(userSessionsKey(user.id), JSON.stringify(index))

  return {
    token,
    session: { id: session.id, expires: session.expires },
    user: { id: user.id, email: user.email },
  }
}

async function sendCodeEmail(env: AuthEnv, email: string, code: string): Promise<void> {
  const response = await fetch('https://api.resend.com/emails', {
    method: 'POST',
    headers: {
      Authorization: `Bearer ${env.RESEND_API_KEY}`,
      'Content-Type': 'application/json',
    },
    body: JSON.stringify({
      from: env.MAIL_FROM ?? 'ObSink <onboarding@resend.dev>',
      to: [email],
      subject: `${code} is your ObSink sign-in code`,
      text:
        `Your ObSink sign-in code is ${code}.\n\n` +
        `It expires in 10 minutes. If you did not request it, ignore this email.\n`,
    }),
  })
  if (!response.ok) {
    throw new AuthError(502, `could not send sign-in email (${response.status})`)
  }
}

function normalizeEmail(value: unknown): string {
  const email = normalizeOptionalEmail(value)
  if (!email) {
    throw new AuthError(400, 'a valid email address is required')
  }
  return email
}

function normalizeOptionalEmail(value: unknown): string | null {
  if (typeof value !== 'string') {
    return null
  }
  const email = value.trim().toLowerCase()
  if (email.length > 254 || !/^[^\s@]+@[^\s@]+\.[^\s@]+$/.test(email)) {
    return null
  }
  return email
}

function randomDigits(length: number): string {
  const bytes = crypto.getRandomValues(new Uint8Array(length))
  return Array.from(bytes, (byte) => String(byte % 10)).join('')
}

export async function sha256Hex(value: string): Promise<string> {
  const digest = await crypto.subtle.digest('SHA-256', new TextEncoder().encode(value))
  return Array.from(new Uint8Array(digest), (byte) => byte.toString(16).padStart(2, '0')).join('')
}

function safeEqual(a: string, b: string): boolean {
  const left = new TextEncoder().encode(a)
  const right = new TextEncoder().encode(b)
  if (left.byteLength !== right.byteLength) {
    return false
  }
  const subtle = crypto.subtle as SubtleCrypto & { timingSafeEqual?: (a: ArrayBuffer, b: ArrayBuffer) => boolean }
  if (typeof subtle.timingSafeEqual === 'function') {
    return subtle.timingSafeEqual(left.buffer as ArrayBuffer, right.buffer as ArrayBuffer)
  }
  let diff = 0
  for (let i = 0; i < left.byteLength; i += 1) {
    diff |= left[i] ^ right[i]
  }
  return diff === 0
}

export function base64UrlEncode(bytes: Uint8Array): string {
  let binary = ''
  for (const byte of bytes) {
    binary += String.fromCharCode(byte)
  }
  return btoa(binary).replace(/\+/g, '-').replace(/\//g, '_').replace(/=+$/, '')
}

function base64UrlDecode(value: string): Uint8Array<ArrayBuffer> {
  const padded = value.replace(/-/g, '+').replace(/_/g, '/') + '='.repeat((4 - (value.length % 4)) % 4)
  const binary = atob(padded)
  const bytes = new Uint8Array(new ArrayBuffer(binary.length))
  for (let i = 0; i < binary.length; i += 1) {
    bytes[i] = binary.charCodeAt(i)
  }
  return bytes
}

function base64UrlDecodeToString(value: string): string {
  return new TextDecoder().decode(base64UrlDecode(value))
}

function nowSeconds(): number {
  return Math.floor(Date.now() / 1000)
}

const sessionKey = (tokenHash: string) => `session:${tokenHash}`
const userKey = (id: string) => `user:${id}`
const userEmailKey = (emailHash: string) => `user_email:${emailHash}`
const userAppleKey = (sub: string) => `user_apple:${sub}`
const userSessionsKey = (userId: string) => `user_sessions:${userId}`
const otpKey = (emailHash: string) => `otp:${emailHash}`
const otpCooldownKey = (emailHash: string) => `otp_rl:${emailHash}`
