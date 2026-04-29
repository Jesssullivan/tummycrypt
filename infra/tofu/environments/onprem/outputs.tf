output "candidate_tailnet_services" {
  description = "Candidate tailnet Service names and hostnames when enabled."
  value = var.enable_tailnet_candidate_services ? {
    nats = {
      service_name = module.tailnet_nats_candidate[0].service_name
      hostname     = module.tailnet_nats_candidate[0].tailscale_hostname
      selector     = local.nats_candidate_selector
    }
    seaweedfs = {
      service_name = module.tailnet_seaweedfs_candidate[0].service_name
      hostname     = module.tailnet_seaweedfs_candidate[0].tailscale_hostname
      selector     = local.seaweedfs_candidate_selector
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

output "stateful_migration_candidate_workloads" {
  description = "Candidate NATS/SeaweedFS workload names, images, selectors, and target PVCs when enabled."
  value = var.enable_stateful_migration_candidate_workloads ? {
    nats = {
      stateful_set = kubernetes_stateful_set_v1.nats_candidate[0].metadata[0].name
      service      = kubernetes_service_v1.nats_candidate[0].metadata[0].name
      image        = var.nats_image
      selector     = local.nats_candidate_selector
      target_pvc   = var.nats_target_pvc_name
    }
    seaweedfs = {
      stateful_set = kubernetes_stateful_set_v1.seaweedfs_candidate[0].metadata[0].name
      service      = kubernetes_service_v1.seaweedfs_candidate[0].metadata[0].name
      image        = var.seaweedfs_image
      selector     = local.seaweedfs_candidate_selector
      target_pvc   = var.seaweedfs_target_pvc_name
    }
  } : {}
}
