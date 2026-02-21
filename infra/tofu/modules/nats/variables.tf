variable "namespace" {
  description = "Kubernetes namespace"
  type        = string
  default     = "tcfs"
}

variable "cluster_size" {
  description = "NATS cluster size: 1 for dev, 3 for prod"
  type        = number
  default     = 3
}

variable "storage_type" {
  description = "JetStream storage backend: file or memory"
  type        = string
  default     = "file"
}

variable "storage_size_gi" {
  description = "JetStream storage PVC size (GiB)"
  type        = number
  default     = 20
}

variable "storage_class" {
  description = "Kubernetes StorageClass for JetStream PVCs"
  type        = string
  default     = "standard"
}

variable "chart_version" {
  description = "NATS Helm chart version"
  type        = string
  default     = "1.2.6"
}
