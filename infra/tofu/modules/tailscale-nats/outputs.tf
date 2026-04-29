output "tailscale_nats_url" {
  description = "NATS client URL via Tailscale MagicDNS"
  value       = "nats://${var.tailscale_hostname}:4222"
}

output "service_name" {
  description = "Kubernetes Service name for the tailnet NATS endpoint"
  value       = module.nats_tailnet.service_name
}

output "tailscale_ip" {
  description = "Tailscale CGNAT IP assigned to the NATS LoadBalancer (populated after operator reconciles)"
  value       = module.nats_tailnet.tailscale_ip
}
