# Civo Kubernetes environment: bitter-darkness-16657317
#
# Deploy the full tcfs stack to Civo K8s.
# SeaweedFS and NATS run in-cluster in the tcfs namespace;
# sync workers + observability also run in K8s.
#
# To deploy:
#   task infra:apply ENV=civo
# Or manually:
#   cd infra/tofu/environments/civo
#   tofu init && tofu apply

terraform {
  required_version = ">= 1.6"
}

# ── Providers ─────────────────────────────────────────────────────────────────

provider "kubernetes" {
  config_path    = var.kubeconfig_path
  config_context = "bitter-darkness-16657317"
}

provider "helm" {
  kubernetes {
    config_path    = var.kubeconfig_path
    config_context = "bitter-darkness-16657317"
  }
}

# ── NATS JetStream ────────────────────────────────────────────────────────────

module "nats" {
  source = "../../modules/nats"

  namespace      = var.namespace
  cluster_size   = 3
  storage_type   = "file"
  storage_size_gi = 20
  storage_class  = "civo-volume"
}

# ── Tailscale NATS exposure (tailnet only, no public IP) ──────────────────────

module "tailscale_nats" {
  source             = "../../modules/tailscale-nats"
  namespace          = var.namespace
  tailscale_hostname = "nats-tcfs"
}

# ── KEDA autoscaler ───────────────────────────────────────────────────────────

module "keda" {
  source = "../../modules/keda"

  namespace         = var.namespace
  nats_url          = module.nats.nats_url
  target_deployment = module.tcfs_backend.worker_deployment_name
  min_replicas      = 1
  max_replicas      = 50
  lag_threshold     = 100
}

# ── tcfs-backend (sync workers) ───────────────────────────────────────────────

module "tcfs_backend" {
  source = "../../modules/tcfs-backend"

  namespace  = var.namespace
  image      = "ghcr.io/tinyland-inc/tcfsd:${var.image_tag}"
  nats_url   = module.nats.nats_url

  # Point workers at the in-cluster SeaweedFS
  s3_endpoint    = "http://seaweedfs.tcfs.svc.cluster.local:8333"
  s3_bucket      = "tcfs"
  s3_region      = "us-east-1"
  s3_secret_name = "seaweedfs-admin"

  worker_replicas       = 1
  worker_concurrency    = 4
  worker_cpu_request    = "500m"
  worker_memory_request = "512Mi"
  worker_cpu_limit      = "2"
  worker_memory_limit   = "2Gi"
}

# ── Observability ─────────────────────────────────────────────────────────────

module "observability" {
  source = "../../modules/observability"

  namespace      = "monitoring"
  tcfs_namespace = var.namespace
  storage_class  = "civo-volume"

  prometheus_retention_days = 15
  grafana_admin_pw          = var.grafana_admin_pw
}
