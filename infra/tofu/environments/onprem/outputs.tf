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
