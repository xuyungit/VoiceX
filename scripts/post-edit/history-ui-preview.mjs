// Isolated browser fixture for the actual Vue history page, not a production
// seed. No requests reach the user's Tauri backend or persisted settings.
// Run: node scripts/post-edit/history-ui-preview.mjs
// Open /history?scenario=empty|edited|long|many|error|save-error
import { createServer } from 'node:http'

const fixture = `
(() => {
  const scenario = new URL(location.href).searchParams.get('scenario') || 'empty';
  let mode = scenario === 'empty' ? 'off' : 'records';
  let callbackId = 0;
  const callbacks = new Map();
  const listeners = new Map();
  const now = Date.now();
  let records = Array.from({ length: scenario === 'many' ? 125 : scenario === 'empty' || scenario === 'error' ? 0 : 2 }, (_, index) => ({
    id: 'fixture-' + index, recordId: 'source-' + index, sessionId: 'session-' + index,
    timestamp: new Date(now - index * 600000).toISOString(),
    lastObservedAt: new Date(now - index * 600000 + 12000).toISOString(),
    appName: 'Codex · 合成样例', originalText: index % 2 ? '把这个返利的时序数据导出。' : '请检查返利的时序数据，导出一份结果。' + (scenario === 'long' ? '这是用于验证长文本展开的合成样例。'.repeat(20) : ''),
    editedText: index % 2 ? '把这个反力的时序数据导出。' : '请检查反力的时序数据，并导出一份结果。' + (scenario === 'long' ? '这是用于验证长文本展开的合成样例。'.repeat(20) : ''),
    stable: true, composition: index % 3 ? 'inactive' : 'unknown', complete: index % 2 === 0, hasNewerUnconfirmedInput: index % 2 === 1, endReason: index % 2 ? 'focus_left' : 'timeout',
    strategy: 'event_with_checkpoints', rangeEvidence: 'before_after_verified',
    insertions: 0, deletions: 0, substitutions: 2, hadIntermediateChanges: true
  }));
  const emit = () => {
    for (const [id, listener] of listeners) if (listener.event === 'post-edit:updated') {
      callbacks.get(listener.handler)?.({ event: listener.event, id, payload: null });
    }
  };
  window.__TAURI_EVENT_PLUGIN_INTERNALS__ = { unregisterListener: () => {} };
  window.__TAURI_INTERNALS__ = {
    metadata: { currentWindow: { label: 'main' }, currentWebview: { label: 'main' } },
    transformCallback: (callback) => { const id = ++callbackId; callbacks.set(id, callback); return id; },
    unregisterCallback: (id) => callbacks.delete(id),
    invoke: async (command, args = {}) => {
      if (command === 'get_post_edit_settings') return { mode };
      if (command === 'set_post_edit_mode') { mode = args.mode; emit(); return { mode }; }
      if (command === 'get_post_edit_records') {
        if (scenario === 'error') throw new Error('Fixture read failure');
        return { records: records.slice(args.offset, args.offset + args.limit), saveError: scenario === 'save-error' ? 'Fixture write failure' : null };
      }
      if (command === 'delete_post_edit_record') { records = records.filter(r => r.id !== args.id); emit(); return; }
      if (command === 'clear_post_edit_records') { records = []; emit(); return; }
      if (command === 'get_settings') return { uiLanguage: 'zh-CN' };
      if (command === 'get_resolved_ui_locale') return 'zh-CN';
      if (command === 'get_history' || command === 'get_input_devices' || command === 'get_recent_target_apps') return [];
      if (command === 'get_usage_stats' || command === 'get_local_usage_stats') return {};
      if (command === 'get_sync_state') return { state: { status: 'disabled' }, deviceId: 'fixture' };
      if (command === 'get_output_mute_status') return { revision: 0, recording: false, support: 'no_device', pendingRestores: [], recoveryError: null };
      if (command === 'get_post_edit_test_status') return { enabled: mode !== 'off', status: mode === 'off' ? 'disabled' : 'ready', reason: 'none', nativeCode: null, insertions: null, deletions: null, substitutions: null };
      if (command === 'plugin:event|listen') { const id = ++callbackId; listeners.set(id, args); return id; }
      if (command === 'plugin:event|unlisten') { listeners.delete(args.eventId); return; }
      if (command === 'apply_hotkey_config' || command === 'save_settings') return;
      console.warn('Unhandled fixture command:', command);
      return null;
    }
  };
})();`

const server = createServer(async (request, response) => {
  try {
    const upstream = await fetch(new URL(request.url, 'http://localhost:1520'))
    const type = upstream.headers.get('content-type') || ''
    response.statusCode = upstream.status
    response.setHeader('Content-Type', type)
    if (type.includes('text/html')) {
      response.end((await upstream.text()).replace('<head>', '<head><script>' + fixture + '</script>'))
    } else response.end(Buffer.from(await upstream.arrayBuffer()))
  } catch (error) {
    response.statusCode = 502
    response.end(String(error))
  }
})
server.listen(1522, '127.0.0.1', () => console.log('History UI fixture: http://127.0.0.1:1522/history'))
for (const signal of ['SIGINT', 'SIGTERM']) process.on(signal, () => server.close())
