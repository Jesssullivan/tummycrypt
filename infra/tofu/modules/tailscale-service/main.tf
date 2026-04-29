# Generic Tailscale operator Service exposure.
#
# This creates a source-owned LoadBalancer Service for a tailnet-only endpoint
# without mutating the selected backend Service/StatefulSet. Use candidate
# hostnames during migrations to avoid colliding with live Tailscale devices.

terraform {
  required_providers {
    kubernetes = {
      source  = "hashicorp/kubernetes"
      version = ">= 2.26"
    }
  }
}

locals {
  proxy_class_annotation = var.proxy_class == "" ? {} : {
    "tailscale.com/proxy-class" = var.proxy_class
  }

  annotations = merge(
    var.extra_annotations,
    {
      "tailscale.com/expose"   = "true"
      "tailscale.com/hostname" = var.tailscale_hostname
    },
    local.proxy_class_annotation,
  )
}

resource "kubernetes_service_v1" "tailnet" {
  metadata {
    name        = var.name
    namespace   = var.namespace
    labels      = var.labels
    annotations = local.annotations
  }

  spec {
    type                = "LoadBalancer"
    load_balancer_class = "tailscale"
    selector            = var.selector

    dynamic "port" {
      for_each = var.ports

      content {
        name        = port.value.name
        port        = port.value.port
        target_port = port.value.target_port
        protocol    = port.value.protocol
      }
    }
  }
}
