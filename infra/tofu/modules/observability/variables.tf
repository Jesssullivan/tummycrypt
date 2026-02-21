variable "namespace" {
  description = "Namespace for observability stack"
  type        = string
  default     = "monitoring"
}

variable "tcfs_namespace" {
  description = "Namespace where tcfs workloads run (for scraping)"
  type        = string
  default     = "tcfs"
}

variable "prometheus_chart_version" {
  description = "kube-prometheus-stack Helm chart version"
  type        = string
  default     = "58.1.3"
}

variable "loki_chart_version" {
  description = "Grafana Loki Helm chart version"
  type        = string
  default     = "6.6.4"
}

variable "grafana_admin_pw" {
  description = "Grafana admin credential"
  type        = string
  sensitive   = true
  default     = "tcfs-changeme"
}

variable "storage_class" {
  description = "StorageClass for Prometheus/Loki PVCs"
  type        = string
  default     = "standard"
}

variable "prometheus_retention_days" {
  description = "Prometheus data retention in days"
  type        = number
  default     = 15
}
