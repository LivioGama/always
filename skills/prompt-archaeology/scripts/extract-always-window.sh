#!/usr/bin/env bash
set -euo pipefail

if [[ $# -ne 3 ]]; then
  cat >&2 <<'USAGE'
Usage:
  extract-always-window.sh <always-log> <start-iso> <end-iso>

Example:
  extract-always-window.sh ~/Library/Logs/Always/always.2026-06-13 2026-06-13T00:47:00Z 2026-06-13T00:58:00Z

Outputs TSV: timestamp, event, value
USAGE
  exit 2
fi

log_file=$1
start_iso=$2
end_iso=$3

if [[ ! -f "$log_file" ]]; then
  echo "Always log not found: $log_file" >&2
  exit 1
fi

jq -r --arg start "$start_iso" --arg end "$end_iso" '
  select(.timestamp >= $start and .timestamp <= $end)
  | select(
      .fields.message == "uds_focused_app_changed"
      or .fields.message == "transcription_received"
      or .fields.message == "transcription_pasted"
      or .fields.message == "transcript ready for pasting"
      or .fields.message == "grammar_patch_aborted_message_submitted"
    )
  | if .fields.message == "uds_focused_app_changed" then
      [.timestamp, "APP", (.fields.bundle // "")]
    elif .fields.message == "transcription_pasted" then
      [.timestamp, "PASTED", (.fields.processed_text // .fields.raw_text // "")]
    elif .fields.message == "transcript ready for pasting" then
      [.timestamp, "READY", (.fields.text // "")]
    else
      [.timestamp, "SPOKEN", (.fields.text // .fields.processed_text // .fields.raw_text // "")]
    end
  | @tsv
' "$log_file"
