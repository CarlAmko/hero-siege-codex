#!/usr/bin/env bash
# hs-capture.sh — snap a Hero Siege tooltip and send it to the planner.
#
# Three ways to fire it:
#   1. KDE custom shortcut → `hs-capture.sh`   (region snap, no focus steal)
#   2. Steam F12 screenshot → run `hs-capture.sh f12-watch` once in the
#      background; every new Steam screenshot is auto-OCR'd into the planner.
#      F12 works in-game because the Steam overlay grabs it before the game.
#   3. `hs-capture.sh file PATH` — send an existing image (testing/bulk)
#
# Every invocation logs to /tmp/hs/hs-capture.log — if a press "does nothing",
# check whether the log got a line: no line = shortcut didn't fire (KDE
# binding problem); line with error = capture/bridge problem.
#
# Recommended KDE binding: Ctrl+Alt+H  (System Settings → Shortcuts →
# Add Command → Command: /home/carl/Documents/GitHub/hero-siege-codex/hs-plan/hs-capture.sh
# then click "Add custom shortcut" and press the combo). Ctrl+C alone is eaten
# by the game; plain C closes the inventory.
set -uo pipefail

BRIDGE="http://127.0.0.1:8787/invoke"
LOG="/tmp/hs/hs-capture.log"
TMP="$(mktemp --suffix=.png)"
MODE="${1:-region}"

log() { echo "$(date +%H:%M:%S) $*" >> "$LOG"; }
notify() { command -v notify-send >/dev/null 2>&1 && notify-send -t 2500 "HS Capture" "$1" || echo "[hs-capture] $1"; }

send_image() {
  local b64
  b64=$(base64 -w0 "$1")
  local out
  out=$(python3 - "$BRIDGE" "$b64" <<'PYEOF' 2>&1
import base64, json, sys, urllib.request
bridge, b64 = sys.argv[1], sys.argv[2]
req = urllib.request.Request(bridge,
    data=json.dumps({'cmd': 'hs_capture', 'args': {'imageBase64': b64, 'source': 'keybind'}}).encode(),
    headers={'content-type': 'application/json'})
try:
    r = json.load(urllib.request.urlopen(req, timeout=120))
except Exception as e:
    print(f"bridge unreachable: {e}"); sys.exit(1)
if r.get('ok'):
    body = r.get('data') or {}
    parse = body.get('parse') or {}
    name = parse.get('baseId') or (body.get('ocrLines') or ['?'])[0] or '?'
    ok = bool(parse.get('equipped'))
    print(("captured: " if ok else "unparsed: ") + name)
    sys.exit(0 if ok else 2)
else:
    print(f"error: {r.get('error')}"); sys.exit(3)
PYEOF
) || true
  log "$out"
  notify "$out"
  case "$out" in captured:*) exit 0 ;; *) exit 1 ;; esac
}

snap() { # $1 = spectacle args
  spectacle -b -n "$@" -o "$TMP" >/dev/null 2>&1
  [[ -s "$TMP" ]]
}

case "$MODE" in
  region)
    log "invoked: region (shortcut fired)"
    if snap -r; then send_image "$TMP"; else log "no image (snap cancelled or failed)"; notify "no image captured"; fi
    ;;
  window)
    log "invoked: window"
    if snap -a; then send_image "$TMP"; else log "no image"; fi
    ;;
  full)
    log "invoked: full"
    if snap -f; then send_image "$TMP"; else log "no image"; fi
    ;;
  file)
    [[ -n "${2:-}" && -s "${2:-}" ]] || { echo "usage: hs-capture.sh file PATH" >&2; exit 2; }
    log "invoked: file $2"
    send_image "$2"
    exit 0
    ;;
  f12-watch)
    # Watch the Steam screenshots dir for Hero Siege (appid 269210) and
    # OCR every new capture. Run once in background; keep playing + F12.
    SS_DIR="$(echo ~/.local/share/Steam/userdata/*/760/remote/269210/screenshots)"
    mkdir -p "$SS_DIR"
    log "f12-watch: watching $SS_DIR"
    notify "F12 watch active — F12 in game captures tooltips"
    last=""
    while true; do
      newest=$(ls -t "$SS_DIR"/*.jpg "$SS_DIR"/*.png 2>/dev/null | head -1)
      if [[ -n "$newest" && "$newest" != "$last" && -s "$newest" ]]; then
        sleep 1  # let Steam finish writing
        newest_size=$(stat -c%s "$newest" 2>/dev/null || echo 0)
        sleep 1
        if [[ "$(stat -c%s "$newest" 2>/dev/null || echo 0)" == "$newest_size" ]]; then
          log "f12: new screenshot $newest"
          send_image "$newest"
          last="$newest"
        fi
      fi
      sleep 1
    done
    ;;
  stop-f12)
    pkill -f "hs-capture.sh f12-watch" 2>/dev/null && log "f12-watch stopped" || log "no f12-watch running"
    ;;
  *)
    echo "usage: hs-capture.sh [region|window|full|file PATH|f12-watch|stop-f12]" >&2
    exit 2
    ;;
esac
rm -f "$TMP"
