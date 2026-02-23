output "fqdn" {
  description = "Fully qualified domain name"
  value       = "${var.subdomain}.${var.domain}"
}

output "record_id" {
  description = "Porkbun DNS record ID"
  value       = porkbun_dns_record.this.id
}
