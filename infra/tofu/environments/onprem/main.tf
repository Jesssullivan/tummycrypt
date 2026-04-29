# Tinyland on-prem TCFS migration surface.
#
# This environment is intentionally inert by default. It records the on-prem
# tailnet exposure contract without adopting or replacing the live singleton
# NATS/SeaweedFS StatefulSets. Enable candidate Services only after the
# read-only preflight and data/storage migration gates are satisfied.

terraform {
  required_version = ">= 1.6"
}

provider "kubernetes" {
  config_path    = var.kubeconfig_path
  config_context = var.kube_context
}

locals {
  common_labels = {
    "app.kubernetes.io/part-of"    = "tcfs"
    "app.kubernetes.io/managed-by" = "opentofu"
    "tummycrypt.dev/environment"   = "onprem"
  }

  nats_candidate_selector = {
    app                          = var.nats_candidate_app_label
    "app.kubernetes.io/instance" = var.nats_candidate_workload_name
  }

  nats_candidate_labels = merge(local.common_labels, local.nats_candidate_selector, {
    "app.kubernetes.io/name"      = "nats"
    "app.kubernetes.io/component" = "messaging"
    "tummycrypt.dev/migration"    = "stateful-openebs-candidate"
  })

  seaweedfs_candidate_selector = {
    app                          = var.seaweedfs_candidate_app_label
    "app.kubernetes.io/instance" = var.seaweedfs_candidate_workload_name
  }

  seaweedfs_candidate_labels = merge(local.common_labels, local.seaweedfs_candidate_selector, {
    "app.kubernetes.io/name"      = "seaweedfs"
    "app.kubernetes.io/component" = "object-storage"
    "tummycrypt.dev/migration"    = "stateful-openebs-candidate"
  })
}

resource "kubernetes_persistent_volume_claim_v1" "nats_target" {
  count = var.enable_stateful_migration_target_pvcs ? 1 : 0

  metadata {
    name      = var.nats_target_pvc_name
    namespace = var.namespace
    labels = merge(local.common_labels, {
      "app.kubernetes.io/name"      = "nats"
      "app.kubernetes.io/component" = "messaging"
      "tummycrypt.dev/migration"    = "stateful-openebs-target"
    })
  }

  spec {
    access_modes       = ["ReadWriteOnce"]
    storage_class_name = var.nats_target_storage_class

    resources {
      requests = {
        storage = var.nats_target_storage_size
      }
    }
  }
}

resource "kubernetes_persistent_volume_claim_v1" "seaweedfs_target" {
  count = var.enable_stateful_migration_target_pvcs ? 1 : 0

  metadata {
    name      = var.seaweedfs_target_pvc_name
    namespace = var.namespace
    labels = merge(local.common_labels, {
      "app.kubernetes.io/name"      = "seaweedfs"
      "app.kubernetes.io/component" = "object-storage"
      "tummycrypt.dev/migration"    = "stateful-openebs-target"
    })
  }

  spec {
    access_modes       = ["ReadWriteOnce"]
    storage_class_name = var.seaweedfs_target_storage_class

    resources {
      requests = {
        storage = var.seaweedfs_target_storage_size
      }
    }
  }
}

resource "kubernetes_service_v1" "nats_candidate" {
  count = var.enable_stateful_migration_candidate_workloads ? 1 : 0

  wait_for_load_balancer = false

  metadata {
    name      = var.nats_candidate_workload_name
    namespace = var.namespace
    labels    = local.nats_candidate_labels
  }

  spec {
    cluster_ip = "None"
    selector   = local.nats_candidate_selector

    port {
      name        = "client"
      port        = 4222
      target_port = 4222
      protocol    = "TCP"
    }

    port {
      name        = "monitor"
      port        = 8222
      target_port = 8222
      protocol    = "TCP"
    }
  }
}

resource "kubernetes_stateful_set_v1" "nats_candidate" {
  count = var.enable_stateful_migration_candidate_workloads ? 1 : 0

  metadata {
    name      = var.nats_candidate_workload_name
    namespace = var.namespace
    labels    = local.nats_candidate_labels
  }

  spec {
    replicas     = 1
    service_name = kubernetes_service_v1.nats_candidate[0].metadata[0].name

    selector {
      match_labels = local.nats_candidate_selector
    }

    template {
      metadata {
        labels = local.nats_candidate_labels
      }

      spec {
        container {
          name  = "nats"
          image = var.nats_image
          args  = ["-js", "-sd", "/data", "-m", "8222"]

          port {
            name           = "client"
            container_port = 4222
            protocol       = "TCP"
          }

          port {
            name           = "monitor"
            container_port = 8222
            protocol       = "TCP"
          }

          readiness_probe {
            http_get {
              path = "/healthz"
              port = 8222
            }
            initial_delay_seconds = 5
            period_seconds        = 10
          }

          resources {
            requests = {
              cpu    = "100m"
              memory = "128Mi"
            }
            limits = {
              cpu    = "500m"
              memory = "512Mi"
            }
          }

          volume_mount {
            name       = "data"
            mount_path = "/data"
          }
        }

        volume {
          name = "data"
          persistent_volume_claim {
            claim_name = var.nats_target_pvc_name
          }
        }
      }
    }
  }
}

resource "kubernetes_service_v1" "seaweedfs_candidate" {
  count = var.enable_stateful_migration_candidate_workloads ? 1 : 0

  wait_for_load_balancer = false

  metadata {
    name      = var.seaweedfs_candidate_workload_name
    namespace = var.namespace
    labels    = local.seaweedfs_candidate_labels
  }

  spec {
    cluster_ip = "None"
    selector   = local.seaweedfs_candidate_selector

    port {
      name        = "master"
      port        = 9333
      target_port = 9333
      protocol    = "TCP"
    }

    port {
      name        = "volume"
      port        = 8080
      target_port = 8080
      protocol    = "TCP"
    }

    port {
      name        = "filer"
      port        = 8888
      target_port = 8888
      protocol    = "TCP"
    }

    port {
      name        = "s3"
      port        = 8333
      target_port = 8333
      protocol    = "TCP"
    }
  }
}

resource "kubernetes_stateful_set_v1" "seaweedfs_candidate" {
  count = var.enable_stateful_migration_candidate_workloads ? 1 : 0

  metadata {
    name      = var.seaweedfs_candidate_workload_name
    namespace = var.namespace
    labels    = local.seaweedfs_candidate_labels
  }

  spec {
    replicas     = 1
    service_name = kubernetes_service_v1.seaweedfs_candidate[0].metadata[0].name

    selector {
      match_labels = local.seaweedfs_candidate_selector
    }

    template {
      metadata {
        labels = local.seaweedfs_candidate_labels
      }

      spec {
        container {
          name  = "seaweedfs"
          image = var.seaweedfs_image
          args = [
            "server",
            "-master.port=9333",
            "-volume.port=8080",
            "-filer",
            "-s3",
            "-s3.port=8333",
            "-dir=/data",
          ]

          port {
            name           = "master"
            container_port = 9333
            protocol       = "TCP"
          }

          port {
            name           = "volume"
            container_port = 8080
            protocol       = "TCP"
          }

          port {
            name           = "filer"
            container_port = 8888
            protocol       = "TCP"
          }

          port {
            name           = "s3"
            container_port = 8333
            protocol       = "TCP"
          }

          readiness_probe {
            http_get {
              path = "/cluster/status"
              port = 9333
            }
            initial_delay_seconds = 10
            period_seconds        = 10
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

          volume_mount {
            name       = "data"
            mount_path = "/data"
          }
        }

        volume {
          name = "data"
          persistent_volume_claim {
            claim_name = var.seaweedfs_target_pvc_name
          }
        }
      }
    }
  }
}

module "tailnet_nats_candidate" {
  count  = var.enable_tailnet_candidate_services ? 1 : 0
  source = "../../modules/tailscale-service"

  namespace          = var.namespace
  name               = "nats-tailnet-candidate"
  tailscale_hostname = var.nats_tailnet_candidate_hostname
  proxy_class        = var.tailscale_proxy_class
  selector           = local.nats_candidate_selector
  labels             = local.nats_candidate_labels

  ports = [
    {
      name        = "client"
      port        = 4222
      target_port = 4222
    },
    {
      name        = "monitor"
      port        = 8222
      target_port = 8222
    },
  ]
}

module "tailnet_seaweedfs_candidate" {
  count  = var.enable_tailnet_candidate_services ? 1 : 0
  source = "../../modules/tailscale-service"

  namespace          = var.namespace
  name               = "seaweedfs-tailnet-candidate"
  tailscale_hostname = var.seaweedfs_tailnet_candidate_hostname
  proxy_class        = var.tailscale_proxy_class
  selector           = local.seaweedfs_candidate_selector
  labels             = local.seaweedfs_candidate_labels

  ports = [
    {
      name        = "master"
      port        = 9333
      target_port = 9333
    },
    {
      name        = "volume"
      port        = 8080
      target_port = 8080
    },
    {
      name        = "filer"
      port        = 8888
      target_port = 8888
    },
    {
      name        = "s3"
      port        = 8333
      target_port = 8333
    },
  ]
}
