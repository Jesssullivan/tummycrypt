variable "kubeconfig_path" {
  description = "Path to kubeconfig file."
  type        = string
  default     = "~/.kube/config"
}

variable "kube_context" {
  description = "Kubernetes context for the Tinyland on-prem cluster."
  type        = string
  default     = "honey"
}

variable "namespace" {
  description = "Kubernetes namespace for TCFS."
  type        = string
  default     = "tcfs"
}

variable "enable_tailnet_candidate_services" {
  description = "Create non-canonical candidate Tailscale Services for pre-cutover smoke."
  type        = bool
  default     = false
}

variable "tailscale_proxy_class" {
  description = "Tailscale operator ProxyClass for honey/sting proxy placement."
  type        = string
  default     = "honey-sting-tailnet"
}

variable "nats_tailnet_candidate_hostname" {
  description = "Candidate NATS tailnet hostname. Keep distinct from the live canonical hostname until cutover."
  type        = string
  default     = "nats-tcfs-candidate"
}

variable "seaweedfs_tailnet_candidate_hostname" {
  description = "Candidate SeaweedFS tailnet hostname. Keep distinct from the live canonical hostname until cutover."
  type        = string
  default     = "seaweedfs-tcfs-candidate"
}
