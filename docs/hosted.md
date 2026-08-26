# ObSink Cloud — operator runbook

"ObSink Cloud" is the operator-run Worker every client offers as the default
backend. It is the same code as a self-hosted Worker plus **accounts**: users
sign in with an emailed one-time code (all platforms) or Sign in with Apple
(iOS), get an `os_…` session bearer, and see only their own vaults. The
operator's `API_KEY` keeps working as a separate principal (spec §4.1).

Content stays end-to-end encrypted: accounts only scope *which encrypted
vaults* a bearer may touch. The operator cannot read anyone's notes.

## 1. Configure the Worker

`wrangler.toml` vars (rendered by `scripts/render-worker-config.sh`, or set as
GitHub repository variables for CI):

| Var | Purpose | Default |
|---|---|---|
| `APPLE_CLIENT_IDS` | Accepted `aud` values for Apple identity tokens — the iOS bundle ID (`com.obsink.ios`), comma-separated if you add a Services ID later | empty = Apple disabled |
| `MAIL_FROM` | From header for sign-in emails | `ObSink <onboarding@resend.dev>` |
| `MAX_VAULTS_PER_USER` | Vaults per account | `10` |
| `MAX_VAULT_BYTES` | Bytes per account vault (sum of live manifest sizes) | `1073741824` |

Secrets (`npx wrangler secret put <NAME>` from `worker/`, or the CI secret of
the same name):

| Secret | Purpose |
|---|---|
| `API_KEY` | Operator bearer. Optional on a pure hosted deployment; keep it for `scripts/verify-*` and CLI admin use. **Never** give it to a tester. |
| `RESEND_API_KEY` | [Resend](https://resend.com) API key. Absent = email sign-in returns `503` and clients hide the email option. |

Resend's free tier covers a beta. Until you verify a sending domain, Resend
only delivers from `onboarding@resend.dev` to the account owner's own address
(403 otherwise); verify a domain and set `MAIL_FROM` before inviting testers.
The live deployment sends from `ObSink <login@resend.spencerjireh.com>`
(domain verified in the shared Resend account; key in the gitignored `.env`).

Local development: `worker/.dev.vars` (gitignored) with
`AUTH_DEV_RETURN_CODE=1` makes `/auth/email/start` return the code in the
response so no email is needed. Never set it on a deployed Worker.

## 2. Sign in with Apple

- The iOS app carries the `com.apple.developer.applesignin` entitlement;
  automatic signing (`scripts/release-ios.sh`) turns the capability on for the
  App ID.
- `APPLE_CLIENT_IDS=com.obsink.ios`. The Worker fetches Apple's JWKS from
  `https://appleid.apple.com/auth/keys` (cached 1 h) and checks signature,
  issuer, audience, and expiry.
- Apple only sends the user's email on the *first* authorization; the Worker
  links an Apple sign-in to an existing email account when they match, and
  later resolves the user by Apple `sub`.
- Users who hide their email get a private-relay address; it is stored as
  their account email.

## 3. Deploy

```bash
set -a; . ./.env; set +a
APPLE_CLIENT_IDS=com.obsink.ios MAIL_FROM='ObSink <login@resend.spencerjireh.com>' \
WORKER_NAME=obsink-worker KV_NAMESPACE_ID=… R2_BUCKET_NAME=obsink-files \
  bash scripts/render-worker-config.sh worker/wrangler.toml
(cd worker && npx wrangler deploy)
printf '%s' "$RESEND_API_KEY" | (cd worker && npx wrangler secret put RESEND_API_KEY)
curl -s https://obsink-worker.spencer-080.workers.dev/   # {"service":"obsink","auth":{"email":true,"apple":true,"api_key":true}}
```

Clients read `GET /` and only show the sign-in methods that are on.

## 4. Operating

- **See accounts / sessions**: no admin UI yet. KV keys `user:<id>`,
  `user_email:<sha256>`, `user_sessions:<id>`, `session:<sha256>` (see
  `docs/architecture.md`). Tokens are stored hashed; an operator cannot
  impersonate a user from KV.
- **Revoke a user**: delete their `user_sessions:<id>` entries' `session:*`
  keys. Users can sign out other devices themselves (`DELETE /auth/sessions/:id`).
- **Account deletion**: users can delete their account from a client
  (`DELETE /auth/account`); it removes sessions, lookups, and every vault's
  blobs/versions/trash/manifests. App Store guideline 5.1.1(v) requires this.
- **Quotas**: raise `MAX_VAULTS_PER_USER` / `MAX_VAULT_BYTES` and redeploy.
  The 50 MB per-file limit (spec §4.4) still applies.
- **Abuse**: one code per email per minute, 5 attempts per code, 10-minute
  code TTL. Sessions expire after 180 days of the token's issue (not sliding).

## 5. Beta testers

1. Add them to the TestFlight group (`scripts/testflight.py`, docs/platforms.md).
2. They open ObSink → Add Vault → **ObSink Cloud** → Sign in with Apple or
   email code → Create Vault. Nothing to hand out.
3. Desktop/CLI on the same account: `obsink login` or the desktop's ObSink Cloud
   sign-in with the same email; Connect lists their vaults.
