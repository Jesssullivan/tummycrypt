output "scaled_object_name" {
  description = "Name of the KEDA ScaledObject"
  value       = "tcfs-sync-worker-scaler"
}

output "keda_namespace" {
  description = "Namespace where KEDA operator is installed"
  value       = var.keda_namespace
}
