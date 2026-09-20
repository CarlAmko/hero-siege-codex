#!/usr/bin/env bash
# Patch the vendored HSPlanner frontend with the capture-inbox feature and
# rebuild the dist. Idempotent: skips work when the marker file exists.
#
#   hs-plan/apply-frontend-patch.sh [--force]
#
# What it changes (via python string edits, no sed escaping pain):
#   1. copies hs-plan/patches/CaptureInboxModal.tsx into the frontend tree
#   2. GearView.tsx: import + state + header button + modal render
#   3. Global hotkey Ctrl+Shift+C opens the inbox (keydown listener in GearView)
set -euo pipefail
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
FE="$ROOT/refs/HSPlanner/frontend"
MARKER="$ROOT/refs/HSPlanner/.capture-inbox-applied"
FORCE="${1:-}"

if [[ -f "$MARKER" && "$FORCE" != "--force" ]]; then
  echo "[patch] already applied ($MARKER) — use --force to re-apply"
  exit 0
fi

cp "$ROOT/hs-plan/patches/CaptureInboxModal.tsx" "$FE/views/gear/CaptureInboxModal.tsx"

python3 - "$FE" <<'PYEOF'
import sys
from pathlib import Path

fe = Path(sys.argv[1])
gv = fe / 'views' / 'gear' / 'GearView.tsx'
src = gv.read_text()

if 'CaptureInboxModal' in src:
    print('[patch] GearView already patched')
else:
    src = src.replace(
        "import ImportScreenshotModal from './ImportScreenshotModal'",
        "import ImportScreenshotModal from './ImportScreenshotModal'\n"
        "import CaptureInboxModal from './CaptureInboxModal'",
    )
    src = src.replace(
        "  const [importOpen, setImportOpen] = useState(false)",
        "  const [importOpen, setImportOpen] = useState(false)\n"
        "  const [inboxOpen, setInboxOpen] = useState(false)",
    )
    # Global hotkey: Ctrl+Shift+C opens the capture inbox.
    anchor = "  const handleModalClose = useCallback(() => setActiveSlot(null), [])"
    hotkey = anchor + """

  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      if (e.ctrlKey && e.shiftKey && (e.key === 'C' || e.key === 'c')) {
        e.preventDefault()
        setInboxOpen((prev) => !prev)
      }
    }
    window.addEventListener('keydown', onKey)
    return () => window.removeEventListener('keydown', onKey)
  }, [])"""
    src = src.replace(anchor, hotkey)
    src = src.replace(
        "import { useCallback, useMemo, useState } from 'react'",
        "import { useCallback, useEffect, useMemo, useState } from 'react'",
    )
    # Header button next to "Import screenshot".
    src = src.replace(
        """              onClick={() => setImportOpen(true)}
            >
              Import screenshot
            </button>""",
        """              onClick={() => setImportOpen(true)}
            >
              Import screenshot
            </button>
            <button
              type="button"
              className="cursor-pointer rounded-[3px] border border-border-2 bg-transparent px-2.5 py-1 font-mono text-[10px] font-semibold uppercase tracking-[0.14em] text-muted transition-colors hover:border-accent-deep hover:text-accent-hot"
              onClick={() => setInboxOpen(true)}
              title="Snapped tooltips from the game keybind (Ctrl+Shift+C here)"
            >
              Capture inbox
            </button>""",
    )
    # Render the inbox modal next to the import modal.
    src = src.replace(
        """      {importOpen && (
        <ImportScreenshotModal""",
        """      {inboxOpen && <CaptureInboxModal onClose={() => setInboxOpen(false)} />}

      {importOpen && (
        <ImportScreenshotModal""",
    )
    gv.write_text(src)
    print('[patch] GearView.tsx patched')

PYEOF

touch "$MARKER"
echo "[patch] done — building dist..."
cd "$ROOT/refs/HSPlanner"
npm run build 2>&1 | tail -4
echo "[patch] rebuilding serve-ui (re-injects shim into fresh dist)..."
cd "$ROOT/hs-plan"
cargo build --release --bin serve-ui 2>&1 | tail -1
echo "[patch] complete"
