#!/usr/bin/env bash
# Keepalive for hs-plan serve-ui (the planner HTTP bridge).
# Restarts the bridge if it exits, max once per 10s to avoid tight crash loops.
BIN="/home/carl/Documents/GitHub/hero-siege-codex/hs-plan/target/release/serve-ui"
LOG="/tmp/hs/serve-ui.log"
DIST="/home/carl/Documents/GitHub/hero-siege-codex/refs/HSPlanner/dist"
mkdir -p /tmp/hs
while true; do
  "$BIN" --port 8787 --dist "$DIST" >> "$LOG" 2>&1
  echo "$(date +%H:%M:%S) serve-ui exited ($?); restarting in 10s" >> "$LOG"
  sleep 10
done
