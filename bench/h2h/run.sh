#!/bin/sh
# run.sh TOOL TASK OUTDIR: one attempt of one tool on one task in a fresh copy
# of tasks/TASK/repo. The Gemini key must already be in the environment
# (GEMINI_API_KEY for Hermes, GOOGLE_GENERATIVE_AI_API_KEY for opencode) or
# REX's 0600 store; this script never reads or prints it.
# Writes OUTDIR/TOOL-TASK.json with pass/fail, exit code and wall seconds.
set -u
here=$(cd "$(dirname "$0")" && pwd)
tool=$1; task=$2; out=$3
model=${H2H_MODEL:-gemini-3.5-flash-lite}
limit=${H2H_TIMEOUT:-1200}
work=$(mktemp -d); cp -r "$here/tasks/$task/repo/." "$work"
prompt=$(cat "$here/tasks/$task/prompt.md")
mkdir -p "$out"; log="$out/$tool-$task.log"
start=$(date +%s)
case $tool in
  opencode)
    # share disabled so no session can be published
    (cd "$work" && OPENCODE_CONFIG_CONTENT='{"share":"disabled","autoupdate":false}' \
      timeout "$limit" opencode run --model "google/$model" "$prompt") >"$log" 2>&1 ;;
  hermes)
    (cd "$work" && timeout "$limit" hermes chat --provider gemini --model "$model" \
      --oneshot --yolo -q "$prompt") >"$log" 2>&1 ;;
  rex)
    (cd "$work" && timeout "$limit" rex-agent --provider gemini --model "$model" \
      --workspace "$work" --auto-approve-in-workspace "$prompt") >"$log" 2>&1 ;;
  *) echo "unknown tool $tool" >&2; exit 2 ;;
esac
code=$?
secs=$(( $(date +%s) - start ))
if "$here/score.sh" "$task" "$work"; then pass=true; else pass=false; fi
rate=$(grep -c -i -E '429|RESOURCE_EXHAUSTED|rate limit' "$log" || true)
printf '{"tool":"%s","task":"%s","model":"%s","pass":%s,"exit":%d,"seconds":%d,"rate_limit_lines":%d}\n' \
  "$tool" "$task" "$model" "$pass" "$code" "$secs" "$rate" >"$out/$tool-$task.json"
cat "$out/$tool-$task.json"
