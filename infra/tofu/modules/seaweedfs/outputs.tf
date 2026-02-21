output "s3_endpoint" {
  description = "SeaweedFS S3 gateway ClusterIP endpoint"
  value       = "http://${kubernetes_service.seaweedfs_s3.metadata[0].name}.${var.namespace}.svc.cluster.local:8333"
}

output "filer_grpc_endpoint" {
  description = "SeaweedFS filer gRPC ClusterIP endpoint"
  value       = "${kubernetes_service.seaweedfs_filer.metadata[0].name}.${var.namespace}.svc.cluster.local:18888"
}

output "master_peers" {
  description = "Comma-separated list of master peer addresses (for volume/filer)"
  value = join(",", [
    for i in range(var.master_replicas) :
    "seaweedfs-master-${i}.seaweedfs-master-headless.${var.namespace}.svc.cluster.local:9333"
  ])
}

output "admin_secret_name" {
  description = "Name of the Kubernetes Secret holding S3 admin credentials"
  value       = kubernetes_secret.seaweedfs_admin.metadata[0].name
}
