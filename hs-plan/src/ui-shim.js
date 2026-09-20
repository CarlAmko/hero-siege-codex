/*!
 * hs-plan ui-bridge shim — lets the stock HSPlanner web UI run without Tauri.
 *
 * Fakes `window.__TAURI_INTERNALS__` so the app's `invoke()` calls from
 * @tauri-apps/api/core are forwarded to the local hs-plan bridge server
 * (`hs-plan serve-ui`, POST /invoke), which runs the exact same command
 * functions upstream's Tauri backend exposed. Injected into the built dist
 * by `hs-plan serve-ui --dist <path>`; runs before any module script.
 */
(function () {
  'use strict'
  if (window.__TAURI_INTERNALS__) return
  var PORT = window.HS_PLAN_BRIDGE_PORT || 8787
  var BASE = 'http://127.0.0.1:' + PORT

  // --- event plumbing (plugin:event|*) -------------------------------------
  // We never emit native events; handlers are registered harmlessly so boot
  // code (App.tsx listens for warmup-progress) doesn't crash.
  var nextId = 1

  function transformCallback(cb, once) {
    var id = nextId++
    void once
    void cb
    return id
  }

  function callBridge(cmd, args) {
    return fetch(BASE + '/invoke', {
      method: 'POST',
      headers: { 'content-type': 'application/json' },
      body: JSON.stringify({ cmd: cmd, args: args || {} }),
    })
      .then(function (r) { return r.json() })
      .then(function (j) {
        if (j && j.ok) return j.data
        throw new Error((j && j.error) || 'hs-plan bridge error')
      })
  }

  function invoke(cmd, args) {
    args = args || {}
    if (cmd === 'plugin:event|listen') return Promise.resolve(nextId++)
    if (cmd === 'plugin:event|unlisten') return Promise.resolve(null)
    if (cmd === 'plugin:deep-link|get_current') return Promise.resolve(null)
    return callBridge(cmd, args)
  }

  window.__TAURI_INTERNALS__ = {
    invoke: invoke,
    transformCallback: transformCallback,
    metadata: {
      currentWindow: { label: 'main' },
      currentWebview: { label: 'main' },
    },
  }
  console.log('[hs-plan] invoke shim active -> ' + BASE)
})()
