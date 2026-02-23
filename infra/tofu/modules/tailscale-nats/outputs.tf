output "tailscale_nats_url" {
  description = "NATS client URL via Tailscale MagicDNS"
  value       = "nats://${var.tailscale_hostname}:4222"
}
