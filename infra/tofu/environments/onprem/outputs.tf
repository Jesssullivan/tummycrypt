output "candidate_tailnet_services" {
  description = "Candidate tailnet Service names and hostnames when enabled."
  value = var.enable_tailnet_candidate_services ? {
    nats = {
      service_name = module.tailnet_nats_candidate[0].service_name
      hostname     = module.tailnet_nats_candidate[0].tailscale_hostname
    }
    seaweedfs = {
      service_name = module.tailnet_seaweedfs_candidate[0].service_name
      hostname     = module.tailnet_seaweedfs_candidate[0].tailscale_hostname
    }
  } : {}
}

output "stateful_migration_target_pvcs" {
  description = "Target retained PVCs for the downtime-gated NATS/SeaweedFS migration when enabled."
  value = var.enable_stateful_migration_target_pvcs ? {
    nats = {
      pvc_name      = kubernetes_persistent_volume_claim_v1.nats_target[0].metadata[0].name
      storage_class = kubernetes_persistent_volume_claim_v1.nats_target[0].spec[0].storage_class_name
      size          = kubernetes_persistent_volume_claim_v1.nats_target[0].spec[0].resources[0].requests.storage
    }
    seaweedfs = {
      pvc_name      = kubernetes_persistent_volume_claim_v1.seaweedfs_target[0].metadata[0].name
      storage_class = kubernetes_persistent_volume_claim_v1.seaweedfs_target[0].spec[0].storage_class_name
      size          = kubernetes_persistent_volume_claim_v1.seaweedfs_target[0].spec[0].resources[0].requests.storage
    }
  } : {}
}
