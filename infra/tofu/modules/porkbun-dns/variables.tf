variable "domain" {
  description = "Base domain (e.g. tummycrypt.dev)"
  type        = string
}

variable "subdomain" {
  description = "Subdomain to create (e.g. nats.tcfs)"
  type        = string
}

variable "record_type" {
  description = "DNS record type"
  type        = string
  default     = "A"
}

variable "content" {
  description = "Record content (IP address or CNAME target)"
  type        = string
}

variable "ttl" {
  description = "TTL in seconds (Porkbun minimum is 600)"
  type        = number
  default     = 600
}
