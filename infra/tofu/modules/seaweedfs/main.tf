# SeaweedFS Kubernetes deployment
#
# Deploys a 3-master + N-volume + 2-filer + S3-gateway cluster.
# Masters form a raft quorum; volume servers attach to masters via
# the headless Service DNS. Filer uses LevelDB metadata (default) or
# Postgres when filer_metadata_backend = "postgres".
#
# Production: set master_replicas=3, volume_replicas=3, volume_size_gi=200
# Development: master_replicas=1, volume_replicas=1, volume_size_gi=10

terraform {
  required_providers {
    kubernetes = {
      source  = "hashicorp/kubernetes"
      version = ">= 2.26"
    }
  }
}

locals {
  master_peers = join(",", [
    for i in range(var.master_replicas) :
    "seaweedfs-master-${i}.seaweedfs-master-headless.${var.namespace}.svc.cluster.local:9333"
  ])
}

# ── Namespace ─────────────────────────────────────────────────────────────────

resource "kubernetes_namespace" "tcfs" {
  metadata {
    name = var.namespace
    labels = {
      "app.kubernetes.io/managed-by" = "opentofu"
      "app.kubernetes.io/part-of"    = "tcfs"
    }
  }
}

# ── S3 Admin Secret (generated if not present) ───────────────────────────────

resource "kubernetes_secret" "seaweedfs_admin" {
  metadata {
    name      = "seaweedfs-admin"
    namespace = var.namespace
  }
  # Credentials must be pre-populated via SOPS decrypt or external-secrets
  # Placeholder values here — override with: -var="s3_admin_key=..." or via SOPS
  data = {
    access_key_id     = "REPLACE_VIA_SOPS"
    secret_access_key = "REPLACE_VIA_SOPS"
  }
  lifecycle {
    ignore_changes = [data]
  }
}

# ── Master: Headless Service (for StatefulSet DNS) ────────────────────────────

resource "kubernetes_service" "seaweedfs_master_headless" {
  metadata {
    name      = "seaweedfs-master-headless"
    namespace = var.namespace
    labels    = { app = "seaweedfs-master" }
  }
  spec {
    cluster_ip = "None"
    selector   = { app = "seaweedfs-master" }
    port {
      name        = "http"
      port        = 9333
      target_port = 9333
    }
    port {
      name        = "grpc"
      port        = 19333
      target_port = 19333
    }
  }
}

# ── Master: ClusterIP Service (for external access) ───────────────────────────

resource "kubernetes_service" "seaweedfs_master" {
  metadata {
    name      = "seaweedfs-master"
    namespace = var.namespace
    labels    = { app = "seaweedfs-master" }
  }
  spec {
    selector = { app = "seaweedfs-master" }
    port {
      name        = "http"
      port        = 9333
      target_port = 9333
    }
  }
}

# ── Master: StatefulSet ───────────────────────────────────────────────────────

resource "kubernetes_stateful_set" "seaweedfs_master" {
  metadata {
    name      = "seaweedfs-master"
    namespace = var.namespace
  }
  spec {
    service_name = kubernetes_service.seaweedfs_master_headless.metadata[0].name
    replicas     = var.master_replicas

    selector {
      match_labels = { app = "seaweedfs-master" }
    }

    template {
      metadata {
        labels = { app = "seaweedfs-master" }
      }
      spec {
        container {
          name  = "master"
          image = "chrislusf/seaweedfs:${var.image_tag}"
          args  = [
            "master",
            "-port=9333",
            "-peers=${local.master_peers}",
            "-mdir=/data",
            "-volumeSizeLimitMB=30000",
          ]
          port {
            name           = "http"
            container_port = 9333
          }
          port {
            name           = "grpc"
            container_port = 19333
          }
          volume_mount {
            name       = "data"
            mount_path = "/data"
          }
          readiness_probe {
            http_get {
              path = "/cluster/status"
              port = 9333
            }
            initial_delay_seconds = 10
            period_seconds        = 10
          }
          liveness_probe {
            http_get {
              path = "/cluster/status"
              port = 9333
            }
            initial_delay_seconds = 30
            period_seconds        = 30
          }
          resources {
            requests = {
              cpu    = "200m"
              memory = "256Mi"
            }
            limits = {
              cpu    = "1"
              memory = "1Gi"
            }
          }
        }
      }
    }

    volume_claim_template {
      metadata {
        name = "data"
      }
      spec {
        access_modes       = ["ReadWriteOnce"]
        storage_class_name = var.storage_class
        resources {
          requests = {
            storage = "10Gi"
          }
        }
      }
    }
  }
}

# ── Volume Server: Service ────────────────────────────────────────────────────

resource "kubernetes_service" "seaweedfs_volume" {
  metadata {
    name      = "seaweedfs-volume"
    namespace = var.namespace
    labels    = { app = "seaweedfs-volume" }
  }
  spec {
    cluster_ip = "None"
    selector   = { app = "seaweedfs-volume" }
    port {
      name        = "http"
      port        = 8080
      target_port = 8080
    }
  }
}

# ── Volume Server: StatefulSet ────────────────────────────────────────────────

resource "kubernetes_stateful_set" "seaweedfs_volume" {
  metadata {
    name      = "seaweedfs-volume"
    namespace = var.namespace
  }
  spec {
    service_name = kubernetes_service.seaweedfs_volume.metadata[0].name
    replicas     = var.volume_replicas

    selector {
      match_labels = { app = "seaweedfs-volume" }
    }

    template {
      metadata {
        labels = { app = "seaweedfs-volume" }
      }
      spec {
        container {
          name  = "volume"
          image = "chrislusf/seaweedfs:${var.image_tag}"
          args  = [
            "volume",
            "-port=8080",
            "-mserver=${kubernetes_service.seaweedfs_master.metadata[0].name}.${var.namespace}.svc.cluster.local:9333",
            "-dir=/data",
            "-max=100",
          ]
          port {
            container_port = 8080
          }
          volume_mount {
            name       = "data"
            mount_path = "/data"
          }
          resources {
            requests = {
              cpu    = "500m"
              memory = "512Mi"
            }
            limits = {
              cpu    = "2"
              memory = "2Gi"
            }
          }
        }
      }
    }

    volume_claim_template {
      metadata {
        name = "data"
      }
      spec {
        access_modes       = ["ReadWriteOnce"]
        storage_class_name = var.storage_class
        resources {
          requests = {
            storage = "${var.volume_size_gi}Gi"
          }
        }
      }
    }
  }
}

# ── Filer: Service ────────────────────────────────────────────────────────────

resource "kubernetes_service" "seaweedfs_filer" {
  metadata {
    name      = "seaweedfs-filer"
    namespace = var.namespace
    labels    = { app = "seaweedfs-filer" }
  }
  spec {
    selector = { app = "seaweedfs-filer" }
    port {
      name        = "http"
      port        = 8888
      target_port = 8888
    }
    port {
      name        = "grpc"
      port        = 18888
      target_port = 18888
    }
  }
}

# ── Filer: Deployment ─────────────────────────────────────────────────────────

resource "kubernetes_deployment" "seaweedfs_filer" {
  metadata {
    name      = "seaweedfs-filer"
    namespace = var.namespace
  }
  spec {
    replicas = var.filer_replicas
    selector {
      match_labels = { app = "seaweedfs-filer" }
    }
    template {
      metadata {
        labels = { app = "seaweedfs-filer" }
      }
      spec {
        container {
          name  = "filer"
          image = "chrislusf/seaweedfs:${var.image_tag}"
          args  = [
            "filer",
            "-port=8888",
            "-master=${kubernetes_service.seaweedfs_master.metadata[0].name}.${var.namespace}.svc.cluster.local:9333",
          ]
          port {
            name           = "http"
            container_port = 8888
          }
          port {
            name           = "grpc"
            container_port = 18888
          }
          resources {
            requests = {
              cpu    = "200m"
              memory = "256Mi"
            }
            limits = {
              cpu    = "1"
              memory = "1Gi"
            }
          }
        }
      }
    }
  }
}

# ── S3 Gateway: Service ───────────────────────────────────────────────────────

resource "kubernetes_service" "seaweedfs_s3" {
  metadata {
    name      = "seaweedfs-s3"
    namespace = var.namespace
    labels    = { app = "seaweedfs-s3" }
  }
  spec {
    selector = { app = "seaweedfs-s3" }
    port {
      name        = "s3"
      port        = 8333
      target_port = 8333
    }
  }
}

# ── S3 Gateway: Deployment ────────────────────────────────────────────────────

resource "kubernetes_deployment" "seaweedfs_s3" {
  metadata {
    name      = "seaweedfs-s3"
    namespace = var.namespace
  }
  spec {
    replicas = 2
    selector {
      match_labels = { app = "seaweedfs-s3" }
    }
    template {
      metadata {
        labels = { app = "seaweedfs-s3" }
      }
      spec {
        container {
          name  = "s3"
          image = "chrislusf/seaweedfs:${var.image_tag}"
          args  = [
            "s3",
            "-port=8333",
            "-filer=${kubernetes_service.seaweedfs_filer.metadata[0].name}.${var.namespace}.svc.cluster.local:8888",
          ]
          port {
            container_port = 8333
          }
          resources {
            requests = {
              cpu    = "200m"
              memory = "256Mi"
            }
            limits = {
              cpu    = "1"
              memory = "512Mi"
            }
          }
        }
      }
    }
  }
}
