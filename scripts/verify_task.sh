#!/bin/bash
# verify_task.sh <task_id> <docker_image>
# Runs the DeepSWE verifier inside the task's Docker container.
# Expects model.patch at /tmp/deep-swe-verify/<task_id>/model.patch
#
# Exit codes: 0 = verifier reported REWARD=1, 1 = REWARD absent or 0,
# 2 = model patch failed to apply, 3 = docker pull/run failed.
set -uo pipefail

TASK_ID="$1"
IMAGE="$2"
TASKS_DIR="${TASKS_DIR:-$HOME/deep-swe/tasks}"
WORK_DIR="/tmp/deep-swe-verify/$TASK_ID"

mkdir -p "$WORK_DIR"
RESULT_FILE="$WORK_DIR/result.txt"

echo "[$TASK_ID] Pulling image..."
if ! docker pull "$IMAGE" 2>&1 | tail -1; then
  echo "[$TASK_ID] docker pull failed for $IMAGE" >&2
  exit 3
fi

echo "[$TASK_ID] Running verifier..."
# The verifier's own exit status is captured in EC inside the container (no
# `set -e` around it) so the REWARD block always runs and failing runs still
# produce a parseable result file.
docker run --rm \
  --platform linux/amd64 \
  -v "$WORK_DIR/model.patch:/model.patch:ro" \
  -v "$TASKS_DIR/$TASK_ID/tests/test.patch:/tests/test.patch:ro" \
  -v "$TASKS_DIR/$TASK_ID/tests/test.sh:/verify.sh:ro" \
  "$IMAGE" \
  bash -c '
    mkdir -p /logs/verifier /logs/artifacts
    cd /app
    git apply --whitespace=nowarn /model.patch 2>/dev/null || { echo "PATCH_FAILED"; exit 2; }
    if [ -f /tests/test.patch ]; then
      git apply --whitespace=nowarn /tests/test.patch 2>/dev/null || { echo "PATCH_FAILED"; exit 2; }
    fi
    bash /verify.sh > /logs/verifier/output.txt 2>&1
    EC=$?
    if [ "$EC" -ne 0 ] && [ ! -f /logs/verifier/reward.txt ]; then
      echo "VERIFIER_EXIT=$EC"
    fi
    if [ -f /logs/verifier/reward.txt ]; then
      REWARD=$(cat /logs/verifier/reward.txt)
      echo "REWARD=$REWARD"
    else
      # Extract from output
      if grep -q "New tests exit code: 0" /logs/verifier/output.txt && \
         grep -q "Baseline exit code: 0" /logs/verifier/output.txt; then
        echo "REWARD=1"
      else
        echo "REWARD=0"
      fi
    fi
    echo "---OUTPUT_TAIL---"
    tail -30 /logs/verifier/output.txt
  ' > "$RESULT_FILE" 2>&1
RUN_STATUS=$?

echo "[$TASK_ID] Done. Result:"
grep -E 'REWARD|FAILED|PATCH_FAILED|passed' "$RESULT_FILE" || true

if [ "$RUN_STATUS" -ne 0 ]; then
  echo "[$TASK_ID] docker run exited with status $RUN_STATUS" >&2
  exit 3
fi
if grep -q "PATCH_FAILED" "$RESULT_FILE"; then
  exit 2
fi
if grep -q "REWARD=1" "$RESULT_FILE"; then
  exit 0
fi
exit 1
