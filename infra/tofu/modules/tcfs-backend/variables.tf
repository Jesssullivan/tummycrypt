variable "namespace" {
  description = "Kubernetes namespace"
  type        = string
  default     = "tcfs"
}

variable "image" {
  description = "tcfsd container image (repository:tag)"
  type        = string
  default     = "ghcr.io/tinyland-inc/tcfsd:latest"
}

variable "worker_replicas" {
  description = "Initial sync-worker replica count (KEDA will override this)"
  type        = number
  default     = 1
}

variable "nats_url" {
  description = "NATS JetStream URL"
  type        = string
}

variable "s3_endpoint" {
  description = "SeaweedFS S3 endpoint URL"
  type        = string
}

variable "s3_bucket" {
  description = "S3 bucket name"
  type        = string
  default     = "tcfs"
}

variable "s3_region" {
  description = "S3 region"
  type        = string
  default     = "us-east-1"
}

variable "s3_secret_name" {
  description = "Name of Kubernetes Secret with access_key_id and secret_access_key"
  type        = string
  default     = "seaweedfs-admin"
}

variable "worker_cpu_request" {
  description = "CPU request per worker pod"
  type        = string
  default     = "500m"
}

variable "worker_memory_request" {
  description = "Memory request per worker pod"
  type        = string
  default     = "512Mi"
}

variable "worker_cpu_limit" {
  description = "CPU limit per worker pod"
  type        = string
  default     = "2"
}

variable "worker_memory_limit" {
  description = "Memory limit per worker pod"
  type        = string
  default     = "2Gi"
}

variable "worker_concurrency" {
  description = "Number of concurrent tasks per worker pod (0 = CPU count)"
  type        = number
  default     = 4
}

variable "enable_monitoring" {
  description = "Create ServiceMonitor (requires kube-prometheus-stack CRDs)"
  type        = bool
  default     = true
}
