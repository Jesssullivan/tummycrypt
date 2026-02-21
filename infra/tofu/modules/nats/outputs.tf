output "nats_url" {
  description = "NATS client URL for tcfsd config (nats://...)"
  value       = "nats://nats.${var.namespace}.svc.cluster.local:4222"
}

output "nats_monitoring_url" {
  description = "NATS HTTP monitoring endpoint"
  value       = "http://nats.${var.namespace}.svc.cluster.local:8222"
}
