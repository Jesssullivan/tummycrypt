variable "namespace" {
  description = "Kubernetes namespace where the Service is created."
  type        = string
}

variable "name" {
  description = "Kubernetes Service name for the Tailscale-exposed endpoint."
  type        = string
}

variable "tailscale_hostname" {
  description = "Tailnet hostname assigned by the Tailscale operator."
  type        = string
}

variable "proxy_class" {
  description = "Optional Tailscale ProxyClass name for proxy pod placement."
  type        = string
  default     = ""
}

variable "selector" {
  description = "Service selector for the backend pods."
  type        = map(string)
}

variable "ports" {
  description = "Service ports to expose through Tailscale."
  type = list(object({
    name        = string
    port        = number
    target_port = number
    protocol    = optional(string, "TCP")
  }))
}

variable "labels" {
  description = "Labels applied to the generated Service."
  type        = map(string)
  default     = {}
}

variable "extra_annotations" {
  description = "Additional annotations applied before Tailscale annotations."
  type        = map(string)
  default     = {}
}
