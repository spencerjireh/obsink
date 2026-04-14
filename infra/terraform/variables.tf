variable "cloudflare_account_id" {
  description = "Cloudflare account ID that owns the ObSink Worker resources."
  type        = string
}

variable "worker_name" {
  description = "Worker service name used by Wrangler deploys."
  type        = string
}

variable "r2_bucket_name" {
  description = "R2 bucket name for encrypted file blobs."
  type        = string
}

variable "kv_namespace_title" {
  description = "KV namespace title for vault metadata and manifests."
  type        = string
}

variable "max_batch_inline_bytes" {
  description = "Maximum inline batch payload size exposed to the Worker config."
  type        = number
  default     = 52428800
}
