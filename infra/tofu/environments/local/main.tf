# Local Kubernetes environment (k3s / kind / minikube)
#
# Lighter resource footprint for development.
# Uses the default kubeconfig context.
#
# To deploy:
#   task infra:apply ENV=local
# Or:
#   cd infra/tofu/environments/local
#   tofu init && tofu apply

terraform {
  required_version = ">= 1.6"
}

# ── Providers ─────────────────────────────────────────────────────────────────

provider "kubernetes" {
  config_path = var.kubeconfig_path
}

provider "helm" {
  kubernetes {
    config_path = var.kubeconfig_path
  }
}

# ── NATS JetStream (single-node for dev) ──────────────────────────────────────

module "nats" {
  source = "../../modules/nats"

  namespace       = var.namespace
  cluster_size    = 1
  storage_type    = "memory"
  storage_size_gi = 1
  storage_class   = "standard"
}

# ── KEDA ──────────────────────────────────────────────────────────────────────

module "keda" {
  source = "../../modules/keda"

  namespace         = var.namespace
  nats_url          = module.nats.nats_url
  target_deployment = module.tcfs_backend.worker_deployment_name
  min_replicas      = 1
  max_replicas      = 5
  lag_threshold     = 10
}

# ── tcfs-backend ──────────────────────────────────────────────────────────────

module "tcfs_backend" {
  source = "../../modules/tcfs-backend"

  namespace  = var.namespace
  image      = "ghcr.io/tummycrypt/tcfsd:${var.image_tag}"
  nats_url   = module.nats.nats_url

  s3_endpoint    = "http://localhost:8333"
  s3_bucket      = "tcfs-dev"
  s3_region      = "us-east-1"
  s3_secret_name = "seaweedfs-admin"

  worker_replicas       = 1
  worker_concurrency    = 2
  worker_cpu_request    = "200m"
  worker_memory_request = "256Mi"
  worker_cpu_limit      = "1"
  worker_memory_limit   = "512Mi"
}

# Observability is optional in local dev — uncomment to enable:
# module "observability" {
#   source        = "../../modules/observability"
#   namespace     = "monitoring"
#   tcfs_namespace = var.namespace
# }
