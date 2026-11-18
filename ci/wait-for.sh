#!/usr/bin/env bash
# Wait until FILE contains a line matching PATTERN (grep -E), while the
# process PID is alive. Prints the matching line.
#
#   ci/wait-for.sh <pid> <file> <pattern> [timeout secs, default 20]
set -uo pipefail
pid=$1 file=$2 pattern=$3 timeout=${4:-20}
for _ in $(seq 1 $((timeout * 5))); do
  if line=$(grep -E -m1 "$pattern" "$file" 2>/dev/null); then
    echo "$line"
    exit 0
  fi
  if ! kill -0 "$pid" 2>/dev/null; then
    echo "process $pid exited before \"$pattern\" appeared in $file:"
    cat "$file" 2>/dev/null
    exit 1
  fi
  sleep 0.2
done
echo "no \"$pattern\" in $file within ${timeout}s:"
cat "$file" 2>/dev/null
exit 1
