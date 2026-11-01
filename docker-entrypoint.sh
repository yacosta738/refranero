#!/bin/sh
# Container entrypoint for refranero.
#
# The CLI schedules its catalog refresh as a detached background worker and
# then exits. Inside a container that is not enough: when PID 1 exits the
# runtime stops the container and the still-running worker is killed before it
# can finish, so the automatic refresh never completes.
#
# Running the CLI as a child and then waiting for that worker keeps the
# container alive until the refresh is done. The worker writes to its own log
# file, so its output never mixes with the command output.
set -u

REFRANERO_BIN=/usr/local/bin/refranero
# Upper bound for the background refresh (5 categories, 5s HTTP timeout each).
WORKER_TIMEOUT=120

refranero "$@"
status=$?

# Wait for the detached refresh worker, when one was started. Rust's spawn()
# returns only after the child has exec'd, so the worker is already visible in
# /proc by the time the CLI above returns.
waited=0
while [ "$waited" -lt "$WORKER_TIMEOUT" ]; do
    worker=""
    for proc in /proc/[0-9]*; do
        [ -r "$proc/cmdline" ] || continue
        cmdline=$(tr '\0' ' ' < "$proc/cmdline" 2>/dev/null) || continue
        case "$cmdline" in
            "$REFRANERO_BIN "*refresh-worker*) worker=$proc; break ;;
        esac
    done
    [ -z "$worker" ] && break
    sleep 1
    waited=$((waited + 1))
done

if [ "$waited" -ge "$WORKER_TIMEOUT" ] && [ -n "$worker" ]; then
    echo "refranero: timed out waiting for the catalog refresh worker after ${WORKER_TIMEOUT}s" >&2
fi

exit $status
