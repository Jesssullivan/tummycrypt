output "tailscale_nats_url" {
  description = "NATS client URL via Tailscale MagicDNS"
  value       = "nats://${var.tailscale_hostname}:4222"
}

output "tailscale_ip" {
  description = "Tailscale CGNAT IP assigned to the NATS LoadBalancer (populated after operator reconciles)"
  value       = try(
    [for ingress in kubernetes_service_v1.nats_tailscale.status[0].load_balancer[0].ingress : ingress.ip if ingress.ip != ""][0],
    ""
  )
}
