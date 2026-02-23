# Tailscale-only NATS exposure
#
# Creates a LoadBalancer Service that the Tailscale operator picks up,
# exposing NATS to the tailnet without a public IP.
#
# Prerequisites:
#   - Tailscale operator installed on the cluster
#   - NATS deployed via the nats module (pods labelled app.kubernetes.io/name=nats)
#
# Lab machines connect via MagicDNS:
#   nats://nats-tcfs:4222
# Or via DNS alias (if configured in Tailscale admin):
#   nats://nats.tcfs.tinyland.dev:4222

terraform {
  required_providers {
    kubernetes = {
      source  = "hashicorp/kubernetes"
      version = ">= 2.26"
    }
  }
}

resource "kubernetes_service_v1" "nats_tailscale" {
  metadata {
    name      = "nats-tailscale"
    namespace = var.namespace

    annotations = {
      "tailscale.com/expose"   = "true"
      "tailscale.com/hostname" = var.tailscale_hostname
    }
  }

  spec {
    type                = "LoadBalancer"
    load_balancer_class = "tailscale"

    selector = {
      "app.kubernetes.io/name" = "nats"
    }

    port {
      name        = "client"
      port        = 4222
      target_port = 4222
      protocol    = "TCP"
    }
  }
}
