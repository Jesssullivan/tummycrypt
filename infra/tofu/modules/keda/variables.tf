variable "namespace" {
  description = "Namespace where KEDA watches for ScaledObjects"
  type        = string
  default     = "tcfs"
}

variable "keda_namespace" {
  description = "Namespace to install KEDA operator into"
  type        = string
  default     = "keda"
}

variable "chart_version" {
  description = "KEDA Helm chart version"
  type        = string
  default     = "2.14.0"
}

variable "target_deployment" {
  description = "Name of the sync-worker Deployment to scale"
  type        = string
  default     = "tcfs-sync-worker"
}

variable "min_replicas" {
  description = "Minimum worker pod count"
  type        = number
  default     = 1
}

variable "max_replicas" {
  description = "Maximum worker pod count"
  type        = number
  default     = 100
}

variable "lag_threshold" {
  description = "NATS consumer lag per replica before scaling up"
  type        = number
  default     = 100
}

variable "cooldown_period_seconds" {
  description = "Scale-down cooldown period in seconds"
  type        = number
  default     = 300
}

variable "nats_url" {
  description = "NATS server URL for KEDA trigger"
  type        = string
}

variable "nats_consumer_name" {
  description = "JetStream durable consumer name"
  type        = string
  default     = "sync-workers"
}

variable "nats_stream_name" {
  description = "JetStream stream name"
  type        = string
  default     = "SYNC_TASKS"
}

variable "enable_autoscaling" {
  description = "Create ScaledObject (requires KEDA CRDs — set false on first apply)"
  type        = bool
  default     = true
}
