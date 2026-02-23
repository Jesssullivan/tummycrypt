# NATS JetStream deployment via official Helm chart
#
# Configures:
#   - JetStream with persistent file storage
#   - 3-node cluster for HA (set cluster_size=1 for dev)
#   - Monitoring endpoint on :8222 (scraped by Prometheus via ServiceMonitor)
#   - SYNC_TASKS, HYDRATION_EVENTS, STATE_UPDATES streams created by tcfsd

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

resource "helm_release" "nats" {
  name       = "nats"
  repository = "https://nats-io.github.io/k8s/helm/charts"
  chart      = "nats"
  version    = var.chart_version
  namespace  = var.namespace

  values = [
    yamlencode({
      config = {
        cluster = {
          enabled  = var.cluster_size > 1
          replicas = var.cluster_size
        }
        jetstream = {
          enabled  = true
          fileStore = {
            enabled = var.storage_type == "file"
            pvc = {
              enabled      = var.storage_type == "file"
              size         = "${var.storage_size_gi}Gi"
              storageClass = var.storage_class
            }
          }
          memoryStore = {
            enabled = var.storage_type == "memory"
            maxSize = "4Gi"
          }
        }
      }
      container = {
        image = {
          repository = "nats"
          tag        = "2.10-alpine"
        }
      }
      reloader = {
        enabled = true
      }
      natsBox = {
        enabled = false
      }
      # Prometheus metrics via NATS surveyor endpoint
      promExporter = {
        enabled = true
        port    = 7777
      }
    })
  ]
}

# ServiceMonitor for Prometheus scraping (if kube-prometheus-stack is installed)
resource "kubernetes_manifest" "nats_service_monitor" {
  count = var.enable_monitoring ? 1 : 0

  manifest = {
    apiVersion = "monitoring.coreos.com/v1"
    kind       = "ServiceMonitor"
    metadata = {
      name      = "nats"
      namespace = var.namespace
      labels    = { "app.kubernetes.io/name" = "nats" }
    }
    spec = {
      selector = {
        matchLabels = { "app.kubernetes.io/name" = "nats" }
      }
      endpoints = [{
        port     = "prom-metrics"
        interval = "30s"
      }]
    }
  }
}
