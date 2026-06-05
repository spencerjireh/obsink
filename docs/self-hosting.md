# Self-Hosting Guide

ObSink runs entirely on your own Cloudflare account. This guide takes you from zero to a deployed Worker that the CLI and desktop app can sync against.

You need:

- A Cloudflare account (the free plan is enough to start; R2 requires adding a payment method but has a generous free tier).
- [Node.js](https://nodejs.org) 20+ and [Rust](https://rustup.rs) (stable).
- The [Wrangler](https://developers.cloudflare.com/workers/wrangler/) CLI (installed via the worker's dev dependencies).

## 1. Authenticate Wrangler

```bash
cd worker
npm ci
npx wrangler login          # opens a browser for OAuth
npx wrangler whoami         # confirm your account + permissions
```

You need `workers` and `workers_kv` write permissions; R2 must be enabled on the account.

## 2. Create the storage primitives

ObSink stores encrypted blobs in **R2** and manifests/metadata in **KV**.

```bash
# From the worker/ directory
npx wrangler kv namespace create META       # note the printed namespace id
npx wrangler r2 bucket create obsink-files
```

> You can also provision these with Terraform — see [deploy.md](deploy.md). Terraform needs a `CLOUDFLARE_API_TOKEN`; the Wrangler OAuth login above does not, which is why the CLI path is simplest for a first deploy.

## 3. Render the Worker config

`worker/wrangler.toml` is generated (and git-ignored) so resource IDs never land in version control:

```bash
# From the repo root
WORKER_NAME=obsink-worker \
KV_NAMESPACE_ID=<the id from step 2> \
R2_BUCKET_NAME=obsink-files \
bash scripts/render-worker-config.sh worker/wrangler.toml
```

## 4. Deploy the Worker

```bash
cd worker
npx wrangler deploy
```

Wrangler prints your Worker URL, e.g. `https://obsink-worker.<subdomain>.workers.dev`. Save it.

## 5. Set the API key secret

Every request must carry `Authorization: Bearer <API_KEY>`. Generate a strong key and store it as a Worker secret:

```bash
API_KEY="$(openssl rand -hex 32)"
echo "$API_KEY"                                   # save this — clients need it
printf '%s' "$API_KEY" | npx wrangler secret put API_KEY
```

Treat this key like a password. Rotate it any time by running `wrangler secret put API_KEY` again (all clients must update).

## 6. Verify the deployment

Two scripts exercise the live endpoint end-to-end:

```bash
export WORKER_URL="https://obsink-worker.<subdomain>.workers.dev"
export WORKER_API_KEY="$API_KEY"

./scripts/verify-worker-deploy.sh        # all endpoints, incl. a real 409, batch, delete
./scripts/verify-cli-deployed-sync.sh    # two-device CLI sync + an interactively resolved conflict
```

Both should print `... verification passed`.

## What gets stored where

- **R2 (`obsink-files`)** — encrypted file blobs, keyed by an opaque per-path token (`<vaultId>/<token>`); plus `_versions/` and `_trash/` prefixes for retained history.
- **KV (`META`)** — the vault list (`vaults`) and one manifest per vault (`manifest:<vaultId>`). Manifests are keyed by path token and contain keyed hashes + the encrypted real path (`encPath`).

The server never sees plaintext paths, file contents, or content-derived hashes. See [architecture.md](architecture.md) for the crypto details.

## Maintenance

- **Version/trash pruning** runs automatically via the Worker's two Cron Triggers (configured in `wrangler.toml`): versions are trimmed to the newest 10 per file and 14 days; trash is purged after 30 days.
- **Resetting a vault** for testing: delete its `manifest:<vaultId>` KV key and the `vaults` key, and remove the corresponding R2 objects. (The wire format is versioned via `PROTOCOL_VERSION`; a format change invalidates old manifests.)
