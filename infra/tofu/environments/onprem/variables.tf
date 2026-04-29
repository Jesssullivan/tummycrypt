variable "kubeconfig_path" {
  description = "Path to kubeconfig file."
  type        = string
  default     = "~/.kube/config"
}

variable "kube_context" {
  description = "Kubernetes context for the Tinyland on-prem cluster."
  type        = string
  default     = "honey"
}

variable "namespace" {
  description = "Kubernetes namespace for TCFS."
  type        = string
  default     = "tcfs"
}

variable "enable_tailnet_candidate_services" {
  description = "Create non-canonical candidate Tailscale Services for pre-cutover smoke."
  type        = bool
  default     = false
}

variable "enable_stateful_migration_target_pvcs" {
  description = "Create retained target PVCs for the downtime-gated NATS/SeaweedFS storage migration."
  type        = bool
  default     = false
}

variable "enable_stateful_migration_candidate_workloads" {
  description = "Create non-canonical candidate NATS/SeaweedFS StatefulSets that mount the retained target PVCs."
  type        = bool
  default     = false
}

variable "nats_target_pvc_name" {
  description = "Distinct target PVC name for the NATS migration. Keep separate from live data-nats-0 until cutover."
  type        = string
  default     = "tcfs-nats-openebs-target"
}

variable "nats_target_storage_class" {
  description = "Retained OpenEBS/ZFS storage class for the NATS target PVC."
  type        = string
  default     = "openebs-bumble-messaging-retain"
}

variable "nats_target_storage_size" {
  description = "Requested size for the NATS target PVC."
  type        = string
  default     = "5Gi"
}

variable "nats_candidate_workload_name" {
  description = "Distinct candidate NATS StatefulSet and headless Service name. Keep separate from live nats."
  type        = string
  default     = "nats-openebs-candidate"
}

variable "nats_candidate_app_label" {
  description = "Distinct app label for the candidate NATS workload and tailnet Service selector."
  type        = string
  default     = "nats-openebs-candidate"
}

variable "nats_image" {
  description = "NATS image for the on-prem candidate workload. Mirrors current live readback until a pinned production tag is selected."
  type        = string
  default     = "nats:2.10-alpine"
}

variable "seaweedfs_target_pvc_name" {
  description = "Distinct target PVC name for the SeaweedFS migration. Keep separate from live data-seaweedfs-0 until cutover."
  type        = string
  default     = "tcfs-seaweedfs-openebs-target"
}

variable "seaweedfs_target_storage_class" {
  description = "Retained OpenEBS/ZFS storage class for the SeaweedFS target PVC."
  type        = string
  default     = "openebs-bumble-s3-retain"
}

variable "seaweedfs_target_storage_size" {
  description = "Requested size for the SeaweedFS target PVC."
  type        = string
  default     = "10Gi"
}

variable "seaweedfs_candidate_workload_name" {
  description = "Distinct candidate SeaweedFS StatefulSet and headless Service name. Keep separate from live seaweedfs."
  type        = string
  default     = "seaweedfs-openebs-candidate"
}

variable "seaweedfs_candidate_app_label" {
  description = "Distinct app label for the candidate SeaweedFS workload and tailnet Service selector."
  type        = string
  default     = "seaweedfs-openebs-candidate"
}

variable "seaweedfs_image" {
  description = "SeaweedFS image for the on-prem candidate workload. Mirrors current live readback until a pinned production tag is selected."
  type        = string
  default     = "chrislusf/seaweedfs:latest"
}

variable "tailscale_proxy_class" {
  description = "Tailscale operator ProxyClass for honey/sting proxy placement."
  type        = string
  default     = "honey-sting-tailnet"
}

variable "nats_tailnet_candidate_hostname" {
  description = "Candidate NATS tailnet hostname. Keep distinct from the live canonical hostname until cutover."
  type        = string
  default     = "nats-tcfs-candidate"
}

variable "seaweedfs_tailnet_candidate_hostname" {
  description = "Candidate SeaweedFS tailnet hostname. Keep distinct from the live canonical hostname until cutover."
  type        = string
  default     = "seaweedfs-tcfs-candidate"
}
