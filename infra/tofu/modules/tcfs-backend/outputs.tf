output "worker_deployment_name" {
  description = "Name of the sync-worker Deployment (referenced by KEDA ScaledObject)"
  value       = kubernetes_deployment.sync_worker.metadata[0].name
}

output "metrics_service_name" {
  description = "Name of the worker metrics Service"
  value       = kubernetes_service.sync_worker_metrics.metadata[0].name
}

output "config_map_name" {
  description = "Name of the tcfsd ConfigMap"
  value       = kubernetes_config_map.tcfsd_config.metadata[0].name
}

output "service_account_name" {
  description = "Name of the tcfsd ServiceAccount"
  value       = kubernetes_service_account.tcfsd.metadata[0].name
}
