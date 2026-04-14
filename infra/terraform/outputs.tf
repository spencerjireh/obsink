output "worker_name" {
  description = "Worker service name used for deploys."
  value       = var.worker_name
}

output "r2_bucket_name" {
  description = "R2 bucket name used by the Worker binding."
  value       = cloudflare_r2_bucket.files.name
}

output "kv_namespace_id" {
  description = "KV namespace ID to feed into generated Wrangler config."
  value       = cloudflare_workers_kv_namespace.meta.id
}

output "max_batch_inline_bytes" {
  description = "Worker var value for inline batch size limits."
  value       = var.max_batch_inline_bytes
}
