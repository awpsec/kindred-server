#!/usr/bin/env bash
set -euo pipefail
stage=$PWD/recipe
mkdir -p "$stage/verification" "$stage/proof" "$stage/server" "$PWD/export"
# Recipe scripts are exact reviewed final-PIN adaptations. Their own source/version/hash guards remain mandatory.
python3 "$stage/proof/verify-bundle.py" > "$stage/proof/remote-bundle-check.log"
python3 "$stage/proof/verify-current-harness.py" > "$stage/proof/remote-harness-check.log"
python3 "$stage/proof/stage-context.py" > "$stage/proof/remote-context-check.log"
sh "$stage/build-image.sh" > "$stage/proof/remote-build.log" 2>&1
sh "$stage/proof/run-image-checks.sh" > "$stage/proof/remote-image-checks.log" 2>&1
image="ghcr.io/awpsec/kindred-server:$VERSION"
docker run --rm --network none --read-only -v "$stage/proof:/proof:ro" -v "$stage/proof/email-fixtures:/fixtures:ro" --entrypoint python3 "$image" /proof/check-provider-helper.py > "$stage/proof/image-provider-helper-check.json"
docker run --rm --network none --read-only -v "$stage/proof:/proof:ro" --entrypoint python3 "$image" /proof/check-current-image-harness.py > "$stage/proof/image-current-harness-check.json"
docker run --rm --network none --read-only -v "$stage/proof:/proof:ro" --entrypoint python3 "$image" /proof/check-image-sdk.py > "$stage/proof/image-sdk-check.json"
python3 tools/scripts/release/remote-image/export.py
