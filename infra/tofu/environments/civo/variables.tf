# infra/tofu/environments/civo/variables.tf
variable "kubeconfig_path" {
  description = "Path to kubeconfig file"
  type        = string
  default     = "~/.kube/config"
}

variable "namespace" {
  description = "Kubernetes namespace"
  type        = string
  default     = "tcfs"
}

variable "image_tag" {
  description = "tcfsd container image tag"
  type        = string
  default     = "latest"
}

variable "grafana_admin_pw" {
  description = "Grafana admin credential - override via tfvars.enc.yaml"
  type        = string
  default     = "tcfs-changeme"
  sensitive   = true
}

variable "dns_domain" {
  description = "Base domain for DNS records"
  type        = string
  default     = "tummycrypt.dev"
}

variable "enable_crds" {
  description = "Create CRD-based resources (ServiceMonitor, ScaledObject). Set false on first apply before operators install CRDs."
  type        = bool
  default     = true
}
