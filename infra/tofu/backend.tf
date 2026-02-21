terraform {
  # SeaweedFS S3-compatible state backend
  # Requires: SEAWEED_ACCESS_KEY, SEAWEED_SECRET_KEY env vars
  # Or use local backend for development (see environments/local/)
  backend "s3" {
    bucket = "tcfs-tofu-state"
    key    = "terraform.tfstate"
    region = "us-east-1"

    # SeaweedFS S3 gateway endpoint
    endpoint = "http://dees-appu-bearts:8333"

    skip_credentials_validation = true
    skip_metadata_api_check     = true
    skip_region_validation      = true
    force_path_style            = true
  }
}
