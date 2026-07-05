#!/usr/bin/env bash
#
# deploy-artifact.sh — watch a local HTML file and re-publish it to a public
# GCS bucket every time it changes (i.e. every time the artifact is regenerated).
#
# Assumptions (per the request): gcloud is installed & authenticated, and the
# target file exists. The bucket is assumed to already exist and be public;
# pass --ensure-bucket to create + publicize it if it doesn't.
#
# Usage:
#   ./deploy-artifact.sh                 # deploy once, then watch for changes
#   ./deploy-artifact.sh --once          # deploy a single time and exit
#   ./deploy-artifact.sh --ensure-bucket # create+publicize the bucket if missing
#
# Every setting below can be overridden from the environment, e.g.:
#   FILE=/path/to/other.html BUCKET=my-bucket ./deploy-artifact.sh
#
set -euo pipefail

# ── Config (edit these, or override via env) ───────────────────────────────
# The file to deploy. The dev should point this at their artifact's path.
FILE="${FILE:-/private/tmp/claude-501/-Users-ulpian-Documents-codebase-rift-rift-cli/70fd99b2-d078-4e98-905f-42bb6e98d733/scratchpad/rift-build-status.html}"
BUCKET="${BUCKET:-oneup-491108-rift-build-status}"
PROJECT="${PROJECT:-oneup-491108}"
# Object name in the bucket; defaults to the file's basename so the public URL
# stays stable across regenerations.
OBJECT="${OBJECT:-$(basename "$FILE")}"
CONTENT_TYPE="${CONTENT_TYPE:-text/html}"
# no-cache so a re-published status page shows up immediately for viewers.
CACHE_CONTROL="${CACHE_CONTROL:-no-cache, max-age=0}"
# Seconds between checks when falling back to polling (no fswatch installed).
POLL_INTERVAL="${POLL_INTERVAL:-2}"
# ───────────────────────────────────────────────────────────────────────────

PUBLIC_URL="https://storage.googleapis.com/${BUCKET}/${OBJECT}"
MODE="watch"
ENSURE_BUCKET=0

for arg in "$@"; do
  case "$arg" in
    --once)          MODE="once" ;;
    --ensure-bucket) ENSURE_BUCKET=1 ;;
    -h|--help)
      grep '^#' "$0" | sed 's/^# \{0,1\}//'
      exit 0 ;;
    *) echo "unknown argument: $arg (try --help)" >&2; exit 2 ;;
  esac
done

log() { printf '%s  %s\n' "$(date '+%H:%M:%S')" "$*"; }

# Content hash — the source of truth for "did it actually change" (robust to
# atomic-rename rewrites and to mtime touches that don't change bytes).
file_hash() {
  [[ -f "$FILE" ]] || { echo ""; return; }
  if command -v shasum >/dev/null 2>&1; then
    shasum -a 256 "$FILE" | awk '{print $1}'
  else
    sha256sum "$FILE" | awk '{print $1}'
  fi
}

preflight() {
  command -v gcloud >/dev/null 2>&1 || {
    log "ERROR: gcloud not found on PATH."; exit 1; }

  local acct
  acct="$(gcloud auth list --filter=status:ACTIVE --format='value(account)' 2>/dev/null || true)"
  [[ -n "$acct" ]] || {
    log "ERROR: no active gcloud account. Run: gcloud auth login"; exit 1; }
  log "authed as $acct · project $PROJECT"

  [[ -f "$FILE" ]] || { log "ERROR: file not found: $FILE"; exit 1; }

  if gcloud storage buckets describe "gs://${BUCKET}" --project="$PROJECT" \
        >/dev/null 2>&1; then
    return
  fi

  if [[ "$ENSURE_BUCKET" == "1" ]]; then
    log "bucket gs://${BUCKET} missing — creating + making public"
    gcloud storage buckets create "gs://${BUCKET}" \
      --project="$PROJECT" --location=US --uniform-bucket-level-access
    gcloud storage buckets add-iam-policy-binding "gs://${BUCKET}" \
      --member=allUsers --role=roles/storage.objectViewer >/dev/null
  else
    log "ERROR: bucket gs://${BUCKET} not found."
    log "       Re-run with --ensure-bucket to create it, or create it manually."
    exit 1
  fi
}

deploy() {
  local label="${1:-update}"
  if gcloud storage cp "$FILE" "gs://${BUCKET}/${OBJECT}" \
        --content-type="$CONTENT_TYPE" \
        --cache-control="$CACHE_CONTROL" \
        --project="$PROJECT" --quiet 2>/dev/null; then
    log "deployed (${label}) → ${PUBLIC_URL}"
  else
    # Don't let a transient failure (expired token, network blip) kill the watcher.
    log "WARN: deploy failed — will retry on next change"
  fi
}

LAST_HASH=""
maybe_deploy() {
  local h; h="$(file_hash)"
  [[ -n "$h" ]] || return 0            # file mid-rewrite / momentarily absent
  [[ "$h" != "$LAST_HASH" ]] || return 0
  LAST_HASH="$h"
  deploy "changed"
}

main() {
  preflight

  # Initial publish so the current file is live immediately.
  LAST_HASH="$(file_hash)"
  deploy "initial"

  if [[ "$MODE" == "once" ]]; then
    exit 0
  fi

  trap 'log "stopping"; exit 0' INT TERM

  if command -v fswatch >/dev/null 2>&1; then
    log "watching $FILE via fswatch (Ctrl-C to stop)"
    # Watch the parent dir so atomic-rename rewrites are still caught; dedupe by
    # content hash. Process substitution keeps the loop in this shell so
    # LAST_HASH persists across events.
    while read -r _; do
      sleep 0.3                        # let the writer settle
      maybe_deploy
    done < <(fswatch -o --latency 1 "$(dirname "$FILE")")
  else
    log "fswatch not found — polling every ${POLL_INTERVAL}s (Ctrl-C to stop)"
    log "  (install fswatch for instant deploys:  brew install fswatch)"
    while true; do
      maybe_deploy
      sleep "$POLL_INTERVAL"
    done
  fi
}

main
