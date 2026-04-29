output "service_name" {
  description = "Kubernetes Service name."
  value       = kubernetes_service_v1.tailnet.metadata[0].name
}

output "tailscale_hostname" {
  description = "Tailnet hostname requested for the Service."
  value       = var.tailscale_hostname
}

output "tailscale_ip" {
  description = "Tailscale CGNAT IP assigned to the Service after reconciliation."
  value = try(
    [for ingress in kubernetes_service_v1.tailnet.status[0].load_balancer[0].ingress : ingress.ip if ingress.ip != ""][0],
    ""
  )
}
