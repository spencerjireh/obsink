# Deploying ObSink Worker

This repo uses:

- Terraform for Cloudflare infrastructure
- Wrangler for Worker code deploys
- GitHub Actions for CI and automatic deploys on `main`

## Important Limitation

Terraform state is local right now.

That means GitHub Actions can safely run Terraform validation and speculative plans, but it cannot safely run `terraform apply` on every merge because the workflow runner does not have persistent shared state.

For now, infrastructure changes must be applied locally. Once you move Terraform state to a remote backend, CI can take over apply as well.

## Required GitHub Secrets

- `CLOUDFLARE_API_TOKEN`
- `CLOUDFLARE_ACCOUNT_ID`
- `WORKER_API_KEY`

## Required GitHub Repository Variables

- `WORKER_NAME`
- `R2_BUCKET_NAME`
- `KV_NAMESPACE_TITLE`
- `KV_NAMESPACE_ID`
- `MAX_BATCH_INLINE_BYTES`

`KV_NAMESPACE_ID` comes from Terraform output after the initial local apply.

## Local Bootstrap

1. Export Cloudflare auth locally.

```bash
export CLOUDFLARE_API_TOKEN="..."
export TF_VAR_cloudflare_account_id="..."
```

2. Create local Terraform input values.

```bash
cp infra/terraform/terraform.tfvars.example infra/terraform/terraform.tfvars
```

3. Edit `infra/terraform/terraform.tfvars` with your real production names.

4. Apply infrastructure locally.

```bash
terraform -chdir=infra/terraform init
terraform -chdir=infra/terraform apply
```

5. Capture Terraform outputs.

```bash
terraform -chdir=infra/terraform output
```

6. Set GitHub repository variables from those values.

At minimum:

- `WORKER_NAME`
- `R2_BUCKET_NAME`
- `KV_NAMESPACE_TITLE`
- `KV_NAMESPACE_ID`
- `MAX_BATCH_INLINE_BYTES`

7. Generate a local Wrangler config for manual deploys.

```bash
export WORKER_NAME="$(terraform -chdir=infra/terraform output -raw worker_name)"
export R2_BUCKET_NAME="$(terraform -chdir=infra/terraform output -raw r2_bucket_name)"
export KV_NAMESPACE_ID="$(terraform -chdir=infra/terraform output -raw kv_namespace_id)"
export MAX_BATCH_INLINE_BYTES="$(terraform -chdir=infra/terraform output -raw max_batch_inline_bytes)"
./scripts/render-worker-config.sh worker/wrangler.toml
```

8. Sync the Worker secret locally if needed.

```bash
cd worker
printf '%s' "$WORKER_API_KEY" | npx wrangler secret put API_KEY
```

9. Deploy locally if needed.

```bash
cd worker
npx wrangler deploy
```

## CI Behavior

### Pull requests

- Rust formatting and core tests
- Worker typecheck and tests
- Desktop frontend build and Tauri Rust check
- Terraform fmt, validate, and speculative plan

### Pushes to `main`

- Re-run verification
- Generate `worker/wrangler.toml` from repository variables
- Sync the `API_KEY` Worker secret
- Deploy Worker code with Wrangler

## When To Move Beyond This Setup

Once you want Terraform apply inside GitHub Actions, move state to a remote backend first. Until then, keep Terraform apply local and let CI handle only validation and Worker code deployment.
