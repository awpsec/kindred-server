#!/usr/bin/env bash
set -euo pipefail
stage=$PWD/recipe
phase=setup
report_failure() {
  rc=$?
  trap - EXIT
  if [ "$rc" -ne 0 ]; then
    printf 'remote_image_failed_phase=%s exit_code=%s\n' "$phase" "$rc" >&2
    python3 tools/scripts/release/remote-image/failure.py "$phase" "$rc" || true
  fi
  exit "$rc"
}
trap report_failure EXIT
mkdir -p "$stage/verification" "$stage/proof" "$stage/server" "$PWD/export"
# Recipe scripts are exact reviewed final-PIN adaptations. Their own source/version/hash guards remain mandatory.
phase=bundle
python3 "$stage/proof/verify-bundle.py" > "$stage/proof/remote-bundle-check.log"
phase=harness
python3 "$stage/proof/verify-current-harness.py" > "$stage/proof/remote-harness-check.log"
phase=context
python3 "$stage/proof/stage-context.py" > "$stage/proof/remote-context-check.log"
phase=build
sh "$stage/build-image.sh" > "$stage/proof/remote-build.log" 2>&1
phase=checks
sh "$stage/proof/run-image-checks.sh" > "$stage/proof/remote-image-checks.log" 2>&1
image="ghcr.io/awpsec/kindred-server:$VERSION"
phase=provider
docker run --rm --network none --read-only -v "$stage/proof:/proof:ro" -v "$stage/proof/email-fixtures:/fixtures:ro" --entrypoint python3 "$image" /proof/check-provider-helper.py > "$stage/proof/image-provider-helper-check.json"
phase=current_harness
docker run --rm --network none --read-only -v "$stage/proof:/proof:ro" --entrypoint python3 "$image" /proof/check-current-image-harness.py > "$stage/proof/image-current-harness-check.json"
phase=sdk
docker run --rm --network none --read-only -v "$stage/proof:/proof:ro" --entrypoint python3 "$image" /proof/check-image-sdk.py > "$stage/proof/image-sdk-check.json"
phase=export
python3 tools/scripts/release/remote-image/export.py
