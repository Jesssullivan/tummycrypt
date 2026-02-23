# Porkbun DNS record management
#
# Creates a DNS record via the Porkbun API.
# Credentials: PORKBUN_API_KEY + PORKBUN_SECRET_API_KEY env vars.

terraform {
  required_providers {
    porkbun = {
      source  = "marcfrederick/porkbun"
      version = "~> 1.3"
    }
  }
}

resource "porkbun_dns_record" "this" {
  domain    = var.domain
  subdomain = var.subdomain
  type      = var.record_type
  content   = var.content
  ttl       = var.ttl
}
