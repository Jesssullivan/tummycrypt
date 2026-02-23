variable "namespace" {
  description = "Kubernetes namespace where NATS is deployed"
  type        = string
  default     = "tcfs"
}

variable "tailscale_hostname" {
  description = "Tailnet hostname for the NATS service (resolvable via MagicDNS)"
  type        = string
  default     = "nats-tcfs"
}
