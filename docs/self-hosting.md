# Self-Hosting Guide

ObSink's backend is one Rust binary (`obsink-server`) plus Postgres, run with Docker Compose. This
guide takes you from zero to a server that the CLI, desktop app, and iOS app can sync against.

You need:

- A host with Docker and Docker Compose v2 (a small VPS, a NAS, a home server).
- A domain and TLS. The server speaks plain HTTP on port 8080; put a reverse proxy in front
  (Coolify's Traefik, Caddy, nginx, or Tailscale). iOS refuses plain HTTP to a public host.
- Optional: an SMTP account for email sign-in codes. Without SMTP, iOS users sign in with Apple
  and desktop/CLI users need a dev-only flag (see "Without SMTP").

## 1. Try it locally

```bash
git clone https://github.com/spencerjireh/obsink && cd obsink
docker compose up -d                 # builds the server, starts Postgres and Mailpit
curl -s localhost:8080/              # {"service":"obsink","auth":{...},"invite_required":false}
```

The local stack sets `AUTH_DEV_RETURN_CODE=1` so sign-in codes come back in the API response, and
routes mail to Mailpit at <http://localhost:8025>. Set `OBSINK_PORT=18080` if 8080 is taken.

Sign in from the CLI and create a vault:

```bash
cargo run -p obsink -- login --server-url http://localhost:8080 --email you@example.com
cargo run -p obsink -- init --server-url http://localhost:8080 --vault-name notes \
  --directory ~/Obsidian/notes --passphrase "correct horse battery staple"
```

## 2. Configuration

Every setting is an environment variable. Defaults are in `server/src/config.rs`.

| Variable | Default | Purpose |
|---|---|---|
| `DATABASE_URL` | required | Postgres connection string |
| `OBSINK_DATA_DIR` | `/data` | Blob volume; also holds `server.key` when the key is not set |
| `OBSINK_SERVER_KEY` | generated | 32-byte base64 envelope key (`obsink-server keygen`). **Back it up**: metadata is unreadable without it |
| `OBSINK_API_KEY` | unset | Operator bearer for the admin CLI and `scripts/verify-*`. Unset disables it |
| `SMTP_HOST`, `SMTP_PORT` (587), `SMTP_USERNAME`, `SMTP_PASSWORD`, `SMTP_FROM`, `SMTP_TLS` (`starttls`\|`tls`\|`none`) | unset | Email one-time codes. Unset = email sign-in disabled |
| `APPLE_CLIENT_IDS` | `com.obsink.ios` | Accepted audiences for Sign in with Apple; empty disables it |
| `AUTH_DEV_RETURN_CODE` | unset | `1` returns the email code in the API response. **Never in production** |
| `MAX_VAULTS_PER_USER` | `10` | Per-account vault cap |
| `MAX_VAULT_BYTES` | `1073741824` | Per-vault byte budget for accounts (`507` when exceeded) |
| `MAX_FILE_BYTES` | `52428800` | Hard per-file cap (`413`) |
| `MAX_BATCH_BYTES` | `134217728` | Whole-request cap for `/batch` |
| `RETENTION_INTERVAL_SECS` | `86400` | Version/trash pruning cadence |
| `OBSINK_MIGRATE_ON_START` | `1` | Apply migrations at startup |
| `OBSINK_LISTEN` | `0.0.0.0:8080` | Bind address |
| `RUST_LOG` | `obsink_server=info,tower_http=info` | Log filter |

Sign in with Apple works on any server without Apple-side configuration: the identity token's
audience is the ObSink app's bundle id, which the server verifies against Apple's public keys.

## 3. Deploy with Coolify

1. In Coolify, add a resource of type **Docker Compose** pointing at this repository (branch
   `main`) with the compose file `docker-compose.coolify.yml`. Coolify builds `server/Dockerfile`
   on the host (a Rust release build, several minutes the first time) and runs Postgres
   alongside it. To deploy the prebuilt `ghcr.io/spencerjireh/obsink-server` image instead,
   swap `build:` for `image:` in the compose file; the GHCR package must then be **public**
   (GitHub → Packages → `obsink-server` → Package settings → Change visibility; new packages
   start private and the API cannot change that) or the host needs `docker login ghcr.io`.
2. Set the environment variables in Coolify:
   - `OBSINK_SERVER_KEY` — run `docker run --rm ghcr.io/spencerjireh/obsink-server keygen` and
     paste the output. Store it somewhere safe.
   - `OBSINK_API_KEY` — `openssl rand -hex 32`.
   - `POSTGRES_PASSWORD` — `openssl rand -hex 24`.
   - `SMTP_HOST`, `SMTP_PORT`, `SMTP_USERNAME`, `SMTP_PASSWORD`, `SMTP_FROM`, `SMTP_TLS` — from your
     mail provider.
3. Assign a domain to the `server` service (port 8080). Coolify's Traefik issues the certificate.
4. Deploy. Coolify creates the two named volumes (`obsink-data`, `obsink-pg`); mark them persistent
   in the resource settings so redeploys keep them.
5. Check `https://your-domain/healthz` returns `{"ok":true}` and `https://your-domain/` lists the
   sign-in methods you configured.

Any other compose host works the same way: copy `docker-compose.coolify.yml`, provide the same
environment, and route your proxy to port 8080 of the `server` container.

## 4. First account and invites

The first sign-up on a fresh server needs no invite. After that every new account must present an
invite code; existing accounts sign in freely.

```bash
# You, on the first device (CLI shown; the desktop and iOS apps have the same flow)
obsink login --server-url https://your-domain --email you@example.com

# Mint codes for others (any signed-in user can; codes are single-use, valid 7 days)
obsink invite --server-url https://your-domain
# or from the host, without a session:
docker compose -f docker-compose.coolify.yml exec server obsink-server invite --count 3
```

Friends enter the code in the "Invite code" field when they sign in for the first time.

## 5. Verify

With `.env` holding `OBSINK_SERVER_URL` and `OBSINK_API_KEY` (see `.env.example`):

```bash
set -a; . ./.env; set +a
scripts/verify-server-deploy.sh        # contract: vaults, manifest ETag, conflict 409, batch, invites
scripts/verify-cli-deployed-sync.sh    # two CLI "devices" sync and resolve a conflict
```

Both create and delete their own vaults under the operator principal.

## 6. Without SMTP

If you cannot send mail, iOS users still have Sign in with Apple. For desktop and CLI, run a
one-off local stack with `AUTH_DEV_RETURN_CODE=1` (as `docker-compose.yml` does) so the code is
printed instead of mailed, or mint sessions through an invite from a device that can sign in.
Do not enable `AUTH_DEV_RETURN_CODE` on an internet-facing server: it hands the code to anyone who
knows an email address.

## 7. Backups and upgrades

- **Back up** the Postgres database (`pg_dump`) and the `/data` volume together, plus
  `OBSINK_SERVER_KEY`. Blobs are useless without the database (which maps them to vaults) and both
  are useless without the key. Vault contents stay unreadable without each vault's passphrase in
  any case.
- **Upgrade** by pulling a newer image and redeploying; migrations run at startup
  (`OBSINK_MIGRATE_ON_START=1`). `obsink-server migrate` applies them by hand.
- **Retention** runs at startup and daily; `obsink-server retention` runs one pass and prints what
  it removed.

## What gets stored where

- Postgres: accounts (sealed email/Apple subject), sessions (token hashes), invites, vault rows,
  and one manifest row per file (path token, keyed content hash, size, mtime, tombstone flag,
  encrypted real path).
- `/data/blobs/live/<vault>/…`: current blobs. `/data/blobs/_versions/…` and
  `/data/blobs/_trash/…`: history, pruned by retention.
- `/data/server.key`: the envelope key, only when `OBSINK_SERVER_KEY` is unset.

Everything a client uploads is already AES-256-GCM ciphertext under the vault key; the server wraps
it once more with its own key and never sees plaintext (spec §6).
