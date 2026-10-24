#!/bin/bash
# verify_task.sh <task_id> <docker_image>
# Runs the DeepSWE verifier inside the task's Docker container.
# Expects model.patch at /tmp/deep-swe-verify/<task_id>/model.patch
#
# Exit codes: 0 = verifier reported REWARD=1, 1 = REWARD absent or 0,
# 2 = model patch failed to apply, 3 = docker pull/run failed.
set -uo pipefail

if [ "$#" -ne 2 ]; then
  echo "usage: $0 <task_id> <docker_image>" >&2
  exit 1
fi
TASK_ID="$1"
IMAGE="$2"
# TASK_ID is interpolated into paths and the container name; reject anything
# that is not a single safe path component (also avoids 'unbound variable'
# aborts being read as the 'REWARD absent' exit code).
case "$TASK_ID" in
  ''|*/*|*..*) echo "invalid task_id: $TASK_ID" >&2; exit 1 ;;
esac
TASKS_DIR="${TASKS_DIR:-$HOME/deep-swe/tasks}"
WORK_DIR="/tmp/deep-swe-verify/$TASK_ID"

mkdir -p "$WORK_DIR"
# Per-run result file: concurrent invocations of the same task must not read
# (and exit on) each other's output. result.txt stays as a last-writer-wins
# copy for anything that expects it.
RESULT_FILE="$(mktemp "$WORK_DIR/result.XXXXXX")"
CONTAINER="dswe-verify-${TASK_ID}-$$"
trap 'docker kill "$CONTAINER" >/dev/null 2>&1 || true' EXIT

echo "[$TASK_ID] Pulling image..."
if ! docker pull "$IMAGE" 2>&1 | tail -1; then
  echo "[$TASK_ID] docker pull failed for $IMAGE" >&2
  exit 3
fi

echo "[$TASK_ID] Running verifier..."
# The verifier's own exit status is captured in EC inside the container (no
# `set -e` around it) so the REWARD block always runs and failing runs still
# produce a parseable result file.
# Bounded: a hung verifier cannot park this script (or the container) forever —
# timeout kills the client, the EXIT trap kills the container.
timeout 900 docker run --rm \
  --name "$CONTAINER" \
  --memory 8g --cpus 4 \
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
cp "$RESULT_FILE" "$WORK_DIR/result.txt"

if [ "$RUN_STATUS" -ne 0 ]; then
  echo "[$TASK_ID] docker run exited with status $RUN_STATUS" >&2
  exit 3
fi
# Line-anchored: the verifier's own output tail is in the same file, and a
# stray "REWARD=1" substring inside it must not grade the task as solved.
if grep -qE '^PATCH_FAILED\r?$' "$RESULT_FILE"; then
  exit 2
fi
if grep -qE '^REWARD=1\r?$' "$RESULT_FILE"; then
  exit 0
fi
exit 1
