import { useCallback, useEffect, useRef, useState } from 'react'
import { getItem } from '@data'
import type { EquippedItem, SlotKey } from '../../types'
import {
  MODAL_BTN_CLASS,
  MODAL_BTN_PRIMARY_CLASS,
  MODAL_FOOTER_CLASS,
  Modal,
} from '../../components/ui/Modal'
import { useBuild } from '../../store/build'

/**
 * Capture inbox — receives tooltip screenshots snapped elsewhere (game
 * hotkey → hs-capture.sh → hs-plan serve-ui bridge) while the planner stays
 * untouched in the background. Accumulates one entry per snapped tooltip;
 * each can be equipped into its slot, stashed, or dismissed individually.
 * Also polls any captures queued before the inbox was opened.
 */

interface InboxCapture {
  id: number
  capturedAtMs: number
  source: string
  ocrLines: string[]
  parse: {
    baseId: string | null
    equipped: EquippedItem | null
    lines: { text: string; status: 'matched' | 'ignored' | 'warning'; detail?: string }[]
    errors: string[]
  }
}

const BRIDGE = 'http://127.0.0.1:8787'

export default function CaptureInboxModal({ onClose }: { onClose: () => void }) {
  const [items, setItems] = useState<InboxCapture[]>([])
  const [error, setError] = useState<string | null>(null)
  const [applied, setApplied] = useState<string[]>([])
  const seen = useRef<Set<number>>(new Set())

  const poll = useCallback(async () => {
    try {
      const r = await fetch(`${BRIDGE}/invoke`, {
        method: 'POST',
        headers: { 'content-type': 'application/json' },
        body: JSON.stringify({ cmd: 'hs_captures_take', args: {} }),
      })
      const j = await r.json()
      if (!j.ok) throw new Error(j.error || 'bridge error')
      const fresh = (j.data as InboxCapture[]).filter((c) => !seen.current.has(c.id))
      if (fresh.length > 0) {
        fresh.forEach((c) => seen.current.add(c.id))
        setItems((prev) => [...prev, ...fresh])
      }
      setError(null)
    } catch (err) {
      setError(err instanceof Error ? err.message : String(err))
    }
  }, [])

  useEffect(() => {
    void poll()
    const t = setInterval(poll, 700)
    return () => clearInterval(t)
  }, [poll])

  const apply = (c: InboxCapture, mode: 'equip' | 'stash') => {
    const equipped = c.parse.equipped
    const base = c.parse.baseId ? getItem(c.parse.baseId) : undefined
    if (!equipped || !base) return
    if (mode === 'stash') {
      useBuild.getState().addStashItem(equipped)
    } else {
      const slot = base.slot as SlotKey
      useBuild.getState().commitEquippedItem(slot, equipped)
      useBuild.getState().addStashItem(equipped)
    }
    setApplied((prev) => [...prev, `${mode}:${c.id}`])
    setItems((prev) => prev.filter((x) => x.id !== c.id))
  }

  const dismiss = (id: number) => {
    void fetch(`${BRIDGE}/invoke`, {
      method: 'POST',
      headers: { 'content-type': 'application/json' },
      body: JSON.stringify({ cmd: 'hs_capture_dismiss', args: { id } }),
    })
    setItems((prev) => prev.filter((x) => x.id !== id))
  }

  const baseOf = (c: InboxCapture) => (c.parse.baseId ? getItem(c.parse.baseId) : undefined)

  return (
    <Modal
      onClose={onClose}
      panelClassName="w-[640px] max-w-[94vw]"
      eyebrow="Capture inbox"
      title="Snapped tooltips"
      subtitle="Press the game keybind per item; entries land here for review"
    >
      <div className="flex max-h-[62vh] flex-col gap-3 overflow-y-auto px-6 py-4">
        {error && (
          <p className="m-0 font-mono text-[11px] text-red-400">
            bridge offline: {error}
          </p>
        )}
        {items.length === 0 && !error && (
          <p className="m-0 font-mono text-[11px] text-faint">
            No captures yet — hover an item in game and press your keybind.
          </p>
        )}
        {items.map((c) => {
          const base = baseOf(c)
          const equipped = c.parse.equipped
          const warnings = c.parse.lines.filter((l) => l.status === 'warning')
          const isApplied = applied.includes(`equip:${c.id}`)
          return (
            <section
              key={c.id}
              className="rounded-[3px] border border-border bg-black/20 px-3 py-2"
            >
              <div className="mb-1 flex items-center justify-between gap-2">
                <span className="font-mono text-[11px] font-semibold text-accent-hot">
                  {base?.name ?? c.ocrLines[0] ?? `capture #${c.id}`}
                </span>
                <span className="font-mono text-[10px] uppercase tracking-[0.14em] text-faint">
                  {base?.rarity ?? c.source} · #{c.id}
                </span>
              </div>
              {warnings.length > 0 && (
                <ul className="m-0 mb-1 flex list-none flex-col gap-0.5 p-0 font-mono text-[10px] text-accent-hot">
                  {warnings.map((w, i) => (
                    <li key={i} title={w.detail}>
                      ⚠ {w.text}
                    </li>
                  ))}
                </ul>
              )}
              {c.parse.errors.map((e) => (
                <p key={e} className="m-0 font-mono text-[10px] text-red-400">
                  {e}
                </p>
              ))}
              <div className="mt-1 flex items-center gap-2">
                <button
                  type="button"
                  className={MODAL_BTN_PRIMARY_CLASS}
                  disabled={!equipped || !base || isApplied}
                  onClick={() => apply(c, 'equip')}
                >
                  {base ? `Equip (${base.slot})` : 'Equip'}
                </button>
                <button
                  type="button"
                  className={MODAL_BTN_CLASS}
                  disabled={!equipped || isApplied}
                  onClick={() => apply(c, 'stash')}
                >
                  Stash
                </button>
                <button
                  type="button"
                  className={MODAL_BTN_CLASS}
                  onClick={() => dismiss(c.id)}
                >
                  Dismiss
                </button>
                <span className="font-mono text-[10px] text-faint">
                  {c.ocrLines.length} lines
                </span>
              </div>
            </section>
          )
        })}
      </div>
      <div className={MODAL_FOOTER_CLASS}>
        <span className="font-mono text-[10px] text-faint">
          {items.length} pending
        </span>
        <button type="button" className={MODAL_BTN_PRIMARY_CLASS} onClick={onClose}>
          Done
        </button>
      </div>
    </Modal>
  )
}
