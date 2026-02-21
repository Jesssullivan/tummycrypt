variable "namespace" {
  description = "Kubernetes namespace to deploy SeaweedFS into"
  type        = string
  default     = "tcfs"
}

variable "master_replicas" {
  description = "Number of SeaweedFS master replicas (should be odd: 1 or 3)"
  type        = number
  default     = 3
}

variable "volume_replicas" {
  description = "Number of SeaweedFS volume server replicas"
  type        = number
  default     = 3
}

variable "volume_size_gi" {
  description = "PersistentVolumeClaim size per volume server (GiB)"
  type        = number
  default     = 200
}

variable "storage_class" {
  description = "Kubernetes StorageClass for PVCs"
  type        = string
  default     = "standard"
}

variable "filer_replicas" {
  description = "Number of SeaweedFS filer replicas"
  type        = number
  default     = 2
}

variable "image_tag" {
  description = "SeaweedFS container image tag"
  type        = string
  default     = "3.75"
}
