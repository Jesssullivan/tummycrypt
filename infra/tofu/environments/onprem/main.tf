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

module "tailnet_nats_candidate" {
  count  = var.enable_tailnet_candidate_services ? 1 : 0
  source = "../../modules/tailscale-service"

  namespace          = var.namespace
  name               = "nats-tailnet-candidate"
  tailscale_hostname = var.nats_tailnet_candidate_hostname
  proxy_class        = var.tailscale_proxy_class
  selector           = { app = "nats" }
  labels             = merge(local.common_labels, { "app.kubernetes.io/name" = "nats" })

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
  selector           = { app = "seaweedfs" }
  labels             = merge(local.common_labels, { "app.kubernetes.io/name" = "seaweedfs" })

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
