variable "namespace" {
  description = "Kubernetes namespace where NATS is deployed"
  type        = string
  default     = "tcfs"
}

variable "service_name" {
  description = "Kubernetes Service name for the tailnet NATS endpoint"
  type        = string
  default     = "nats-tailscale"
}

variable "tailscale_hostname" {
  description = "Tailnet hostname for the NATS service (resolvable via MagicDNS)"
  type        = string
  default     = "nats-tcfs"
}

variable "proxy_class" {
  description = "Optional Tailscale ProxyClass name for proxy pod placement"
  type        = string
  default     = ""
}

variable "selector" {
  description = "Selector for the NATS backend pods"
  type        = map(string)
  default = {
    "app.kubernetes.io/name" = "nats"
  }
}
