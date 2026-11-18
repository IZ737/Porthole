#!/usr/bin/env bash
# Startup budget: launch a packed app several times and check the
# launcher's own share of startup (time to a visible window, minus time
# spent waiting for the backend) stays within the budget on a cache hit.
# The first launch unpacks and warms the OS caches, so it is not counted;
# the median of the rest must be within budget.
#
#   ci/startup-budget.sh <packed app> <app id> [budget ms, default 300] [runs, default 5]
#
# Needs a display (xvfb-run) and XDG_DATA_HOME set to a scratch folder.
set -euo pipefail
app=$1 id=$2 budget=${3:-300} runs=${4:-5}
log=$XDG_DATA_HOME/$id/logs/porthole.log
here=$(dirname "$0")
times=()
for i in $(seq 0 "$runs"); do
  rm -f "$log"
  "$app" & pid=$!
  line=$("$here/wait-for.sh" "$pid" "$log" "startup: window shown" 20)
  kill "$pid"; wait "$pid" 2>/dev/null || true
  total=$(sed -E 's/.*shown ([0-9]+) ms.*/\1/' <<<"$line")
  wait_ms=$(sed -E 's/.*, ([0-9]+) ms of it waiting.*/\1/' <<<"$line")
  own=$((total - wait_ms))
  echo "launch $i: window after ${total} ms, ${wait_ms} ms waiting for the backend -> ${own} ms"
  if [ "$i" -gt 0 ]; then times+=("$own"); fi
done
median=$(printf '%s\n' "${times[@]}" | sort -n | sed -n "$(( (${#times[@]} + 1) / 2 ))p")
echo "median of cache-hit launches: ${median} ms (budget ${budget} ms)"
[ "$median" -le "$budget" ]
