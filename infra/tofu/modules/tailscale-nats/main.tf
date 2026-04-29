# Tailscale-only NATS exposure.
#
# This wrapper preserves the historical module API while delegating the Service
# shape to the generic tailscale-service module so on-prem and Civo use the same
# ProxyClass-capable exposure contract.

module "nats_tailnet" {
  source = "../tailscale-service"

  namespace          = var.namespace
  name               = var.service_name
  tailscale_hostname = var.tailscale_hostname
  proxy_class        = var.proxy_class
  selector           = var.selector

  labels = {
    "app.kubernetes.io/name"       = "nats"
    "app.kubernetes.io/component"  = "tailnet"
    "app.kubernetes.io/managed-by" = "opentofu"
  }

  ports = [
    {
      name        = "client"
      port        = 4222
      target_port = 4222
    }
  ]
}
