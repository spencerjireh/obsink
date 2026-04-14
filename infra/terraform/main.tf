resource "cloudflare_r2_bucket" "files" {
  account_id = var.cloudflare_account_id
  name       = var.r2_bucket_name
}

resource "cloudflare_workers_kv_namespace" "meta" {
  account_id = var.cloudflare_account_id
  title      = var.kv_namespace_title
}
