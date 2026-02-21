output "grafana_url" {
  description = "Grafana ClusterIP service URL (use kubectl port-forward to access)"
  value       = "http://kube-prometheus-stack-grafana.${var.namespace}.svc.cluster.local:80"
}

output "prometheus_url" {
  description = "Prometheus ClusterIP service URL"
  value       = "http://kube-prometheus-stack-prometheus.${var.namespace}.svc.cluster.local:9090"
}

output "loki_url" {
  description = "Loki push endpoint"
  value       = "http://loki.${var.namespace}.svc.cluster.local:3100"
}
