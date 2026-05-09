#!/usr/bin/env bash
#
# Regression tests for the release workflow's container image publication
# contract. The current release evidence records an amd64-only image gap; this
# test keeps the next release workflow configured to publish a native arm64
# manifest instead of drifting back to amd64-only.
#
set -euo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
WORKFLOW="${REPO_ROOT}/.github/workflows/release.yml"

# shellcheck disable=SC2016 # Intentional literal assertions against GitHub expressions.
ruby -ryaml -e '
  workflow = YAML.load_file(ARGV[0])
  dispatch = workflow.fetch(true).fetch("workflow_dispatch").fetch("inputs")
  tag = dispatch.fetch("tag")
  raise "release tag input example must track current proof tag" unless tag.fetch("description").include?("v0.12.12")
  raise "release workflow must not default manual dispatch to latest" if tag["default"] == "latest"

  plan_run = workflow.fetch("jobs").fetch("plan").fetch("steps").find { |step| step["name"] == "Determine version" }.fetch("run")
  raise "release plan must validate semantic v-tag input" unless plan_run.include?("Release tag must look like vX.Y.Z")

  steps = workflow.fetch("jobs").fetch("build-image").fetch("steps")

  qemu_index = steps.index { |step| step["uses"] == "docker/setup-qemu-action@v3" }
  buildx_index = steps.index { |step| step["uses"] == "docker/setup-buildx-action@v3" }
  image_index = steps.index { |step| step["name"] == "Build and push image" }

  raise "build-image job must set up QEMU for non-native arm64 builds" unless qemu_index
  raise "build-image job must set up Docker Buildx" unless buildx_index
  raise "Build and push image step not found" unless image_index
  raise "QEMU setup must run before Buildx setup" unless qemu_index < buildx_index
  raise "Buildx setup must run before image build" unless buildx_index < image_index

  qemu = steps.fetch(qemu_index)
  qemu_platforms = qemu.fetch("with").fetch("platforms").to_s.split(",").map(&:strip)
  raise "QEMU setup must enable arm64" unless qemu_platforms.include?("arm64")

  image = steps.fetch(image_index)
  with = image.fetch("with")
  platforms = with.fetch("platforms").to_s.split(",").map(&:strip)
  expected = ["linux/amd64", "linux/arm64/v8"]
  raise "container platforms mismatch: #{platforms.inspect}" unless platforms == expected
  raise "container image must still push release tags" unless with.fetch("push") == true

  tags = with.fetch("tags").to_s
  ["${{ needs.plan.outputs.tag }}", "${{ needs.plan.outputs.version }}", ":latest"].each do |needle|
    raise "container tags missing #{needle}" unless tags.include?(needle)
  end

  sign = steps.find { |step| step["name"] == "Sign container image with Cosign (keyless)" }
  raise "container signing step not found" unless sign
  run = sign.fetch("run")
  raise "container signing must use the immutable build digest" unless run.include?("@${IMAGE_DIGEST}")

  release_steps = workflow.fetch("jobs").fetch("create-release").fetch("steps")
  release = release_steps.find { |step| step["name"] == "Create release" }
  raise "create-release step not found" unless release
  body = release.fetch("with").fetch("body")
  raise "release body must ask operators to verify amd64 image pulls explicitly" unless body.include?("podman pull --arch amd64")
  raise "release body must ask operators to verify arm64 image pulls explicitly" unless body.include?("podman pull --arch arm64")
  raise "release body must state the current Debian floor" unless body.include?("Ubuntu 24.04+ / Debian 13+")
  raise "release body must keep macOS Finder/FileProvider experimental" unless body.include?("Finder/FileProvider remains experimental")
' "$WORKFLOW"

printf 'release workflow container tests passed\n'
