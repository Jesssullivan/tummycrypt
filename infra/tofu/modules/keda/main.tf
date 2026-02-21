# KEDA (Kubernetes Event-Driven Autoscaling) deployment
#
# Installs KEDA and creates a ScaledObject that auto-scales the
# tcfs sync-worker Deployment based on NATS JetStream consumer lag.
#
# Scaling logic:
#   - consumer lag / lag_threshold = desired replicas
#   - min_replicas to max_replicas bounds
#   - cooldown_period_seconds before scale-down

terraform {
  required_providers {
    helm = {
      source  = "hashicorp/helm"
      version = ">= 2.12"
    }
    kubernetes = {
      source  = "hashicorp/kubernetes"
      version = ">= 2.26"
    }
  }
}

# ── KEDA operator ─────────────────────────────────────────────────────────────

resource "kubernetes_namespace" "keda" {
  metadata {
    name = var.keda_namespace
  }
}

resource "helm_release" "keda" {
  name       = "keda"
  repository = "https://kedacore.github.io/charts"
  chart      = "keda"
  version    = var.chart_version
  namespace  = var.keda_namespace

  set {
    name  = "watchNamespace"
    value = var.namespace
  }
}

# ── ScaledObject for sync-worker ──────────────────────────────────────────────

resource "kubernetes_manifest" "sync_worker_scaled_object" {
  depends_on = [helm_release.keda]

  manifest = {
    apiVersion = "keda.sh/v1alpha1"
    kind       = "ScaledObject"
    metadata = {
      name      = "tcfs-sync-worker-scaler"
      namespace = var.namespace
    }
    spec = {
      scaleTargetRef = {
        apiVersion = "apps/v1"
        kind       = "Deployment"
        name       = var.target_deployment
      }
      minReplicaCount  = var.min_replicas
      maxReplicaCount  = var.max_replicas
      cooldownPeriod   = var.cooldown_period_seconds
      pollingInterval  = 15
      triggers = [{
        type = "nats-jetstream"
        metadata = {
          natsServerMonitoringEndpoint = replace(var.nats_url, "nats://", "http://") + ":8222"
          account                      = "$G"
          stream                       = var.nats_stream_name
          consumer                     = var.nats_consumer_name
          lagThreshold                 = tostring(var.lag_threshold)
          activationLagThreshold       = "1"
        }
      }]
    }
  }
}
