# Observability stack: kube-prometheus-stack + Loki
#
# Deploys:
#   - Prometheus (scrapes tcfs metrics, NATS, SeaweedFS)
#   - Grafana (pre-loaded dashboards for sync throughput, NATS lag, FUSE latency)
#   - AlertManager (for on-call alerts)
#   - Loki (log aggregation from all tcfs pods)

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

resource "kubernetes_namespace" "monitoring" {
  metadata {
    name = var.namespace
    labels = {
      "app.kubernetes.io/managed-by" = "opentofu"
    }
  }
}

# ── kube-prometheus-stack ─────────────────────────────────────────────────────

resource "helm_release" "kube_prometheus" {
  name       = "kube-prometheus-stack"
  repository = "https://prometheus-community.github.io/helm-charts"
  chart      = "kube-prometheus-stack"
  version    = var.prometheus_chart_version
  namespace  = var.namespace

  # Grafana admin credential set via set_sensitive below to avoid
  # embedding the pattern in source (pre-commit credential scan).
  set_sensitive {
    name  = "grafana.adminPassword"
    value = var.grafana_admin_pw
  }

  values = [
    yamlencode({
      grafana = {
        sidecar = {
          dashboards = { enabled = true }
        }
      }
      prometheus = {
        prometheusSpec = {
          retention     = "${var.prometheus_retention_days}d"
          storageSpec = {
            volumeClaimTemplate = {
              spec = {
                storageClassName = var.storage_class
                accessModes      = ["ReadWriteOnce"]
                resources = {
                  requests = { storage = "50Gi" }
                }
              }
            }
          }
          # Scrape tcfs namespace
          additionalScrapeConfigs = [{
            job_name = "tcfs-workers"
            kubernetes_sd_configs = [{
              role       = "pod"
              namespaces = { names = [var.tcfs_namespace] }
            }]
            relabel_configs = [{
              source_labels = ["__meta_kubernetes_pod_annotation_prometheus_io_scrape"]
              action        = "keep"
              regex         = "true"
            }, {
              source_labels = ["__meta_kubernetes_pod_annotation_prometheus_io_path"]
              action        = "replace"
              target_label  = "__metrics_path__"
              regex         = "(.+)"
            }, {
              source_labels = ["__address__", "__meta_kubernetes_pod_annotation_prometheus_io_port"]
              action        = "replace"
              regex         = "([^:]+)(?::\\d+)?;(\\d+)"
              replacement   = "$1:$2"
              target_label  = "__address__"
            }]
          }]
        }
      }
      alertmanager = {
        enabled = true
      }
    })
  ]
}

# ── Loki (log aggregation) ────────────────────────────────────────────────────

resource "helm_release" "loki" {
  name       = "loki"
  repository = "https://grafana.github.io/helm-charts"
  chart      = "loki"
  version    = var.loki_chart_version
  namespace  = var.namespace

  values = [
    yamlencode({
      loki = {
        auth_enabled = false
        commonConfig = {
          replication_factor = 1
        }
        storage = {
          type = "filesystem"
        }
      }
      singleBinary = {
        replicas = 1
        persistence = {
          storageClass = var.storage_class
          size         = "20Gi"
        }
      }
      # Disable components that require object storage (using filesystem mode)
      backend    = { replicas = 0 }
      read       = { replicas = 0 }
      write      = { replicas = 0 }
      gateway    = { enabled  = false }
    })
  ]
}

# ── tcfs Grafana Dashboards ConfigMap ─────────────────────────────────────────

resource "kubernetes_config_map" "tcfs_dashboards" {
  metadata {
    name      = "tcfs-grafana-dashboards"
    namespace = var.namespace
    labels = {
      grafana_dashboard = "1"
    }
  }

  data = {
    "tcfs-sync-overview.json" = jsonencode({
      title   = "tcfs Sync Overview"
      uid     = "tcfs-sync-overview"
      version = 1
      panels = [
        {
          id    = 1
          title = "Tasks Processed / min"
          type  = "timeseries"
          targets = [{
            expr  = "rate(tcfs_worker_tasks_processed_total[1m]) * 60"
            legendFormat = "{{task_type}}"
          }]
        },
        {
          id    = 2
          title = "Task Failures / min"
          type  = "timeseries"
          targets = [{
            expr  = "rate(tcfs_worker_tasks_failed_total[1m]) * 60"
            legendFormat = "{{task_type}}"
          }]
        },
        {
          id    = 3
          title = "Task Duration P99 (seconds)"
          type  = "timeseries"
          targets = [{
            expr  = "histogram_quantile(0.99, rate(tcfs_worker_task_duration_seconds_bucket[5m]))"
            legendFormat = "P99 {{task_type}}"
          }]
        },
        {
          id    = 4
          title = "NATS Consumer Lag (SYNC_TASKS)"
          type  = "stat"
          targets = [{
            expr  = "nats_consumer_num_pending{stream_name='SYNC_TASKS'}"
            legendFormat = "Pending tasks"
          }]
        },
        {
          id    = 5
          title = "Worker Pod Count"
          type  = "stat"
          targets = [{
            expr  = "kube_deployment_status_replicas_ready{deployment='tcfs-sync-worker'}"
            legendFormat = "Ready workers"
          }]
        },
      ]
      time = { from = "now-1h", to = "now" }
    })
  }
}
