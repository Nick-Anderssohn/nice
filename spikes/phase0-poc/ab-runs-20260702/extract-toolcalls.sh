#!/bin/bash
# Extract a compact tool-call sequence from a Claude Code subagent JSONL transcript.
# Usage: extract-toolcalls.sh <transcript.jsonl>
# Output: TSV — seq, tool name, compact input summary (first 200 chars).
# Judges use this to count API hallucinations and iterations-to-green.
set -euo pipefail
f="$1"
jq -r '
  select(.type=="assistant")
  | .message.content[]?
  | select(.type=="tool_use")
  | [.name,
     (.input
      | if .command? then .command
        elif .file_path? then ((.file_path|tostring) + (if .old_string? then " [edit]" elif .content? then " [write]" else "" end))
        elif .pattern? then ("grep: " + (.pattern|tostring))
        else (tostring)
        end)
    ]
  | @tsv' "$f" 2>/dev/null \
| awk -F'\t' '{gsub(/\n/," ",$2); printf "%d\t%s\t%.200s\n", NR, $1, $2}'
