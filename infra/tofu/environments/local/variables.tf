# infra/tofu/environments/local/variables.tf
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
