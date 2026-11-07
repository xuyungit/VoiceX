// Isolated browser fixture for the actual Vue history page, not a production
// seed. No requests reach the user's Tauri backend or persisted settings.
// Run: node scripts/post-edit/history-ui-preview.mjs
// Open /history?scenario=empty|edited|diff|long|many|error|save-error|off
import { createServer } from 'node:http'

const fixture = `
(() => {
  const scenario = new URL(location.href).searchParams.get('scenario') || 'empty';
  let mode = scenario === 'off' ? 'off' : 'records';
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
    insertions: 0, deletions: 0, substitutions: 2, hadIntermediateChanges: true, source: 'observed'
  }));
  // Diff display cases: substitution, insertion, mixed-script, a rewrite that
  // falls back to the stacked layout, and a long text that condenses.
  if (scenario === 'diff') records = [
    ['今天开会的记要我已经发到群里了，麻烦帮我看一下有没有问题。', '今天开会的纪要我已经发到群里了，麻烦帮我看一下有没有问题。'],
    ['我做了一个简单的测试，按照之前的设计，最后五条修改的记录应该做替换，所以你检查一下现在这个功能是不是正常的。', '我做了一个简单的测试，按照之前的设计，最后五条修改的记录应该做替换得到正确的识别：DERP，所以你检查一下现在这个功能是不是正常的。'],
    ['那你觉得是提示词写得更密一些好，还是这种方式给大语言模型传递的信息不足以让大语言模型决定去纠正其中的某些单词呢？', '那你觉得是提示词写得更好有可能改善，还是这种方式给大语言模型传递的信息不足以让大语言模型决定去纠正其中的某些单词呢？'],
    ['我们用 cloud code 来写 type script', '我们用 Claude Code 来写 TypeScript'],
    ['今天天气很好，我们出去走走吧。', '明天可能下雨，记得带伞。'],
    ['这是一段很长的听写，' + '用于验证折叠后只显示改动附近的上下文。'.repeat(8) + '这里有一个错别子。' + '后面还有更多内容。'.repeat(8), '这是一段很长的听写，' + '用于验证折叠后只显示改动附近的上下文。'.repeat(8) + '这里有一个错别字。' + '后面还有更多内容。'.repeat(8)]
  ].map(([originalText, editedText], index) => ({ ...records[0], id: 'diff-' + index, recordId: 'diff-source-' + index,
    timestamp: new Date(now - index * 600000).toISOString(), originalText, editedText }));
  // Dictation rows mirror the pairs: an observed edit is the row's final text.
  let history = records.map(r => ({
    id: r.recordId, timestamp: r.timestamp, text: r.originalText, originalText: r.originalText,
    aiCorrectionApplied: false, llmInvoked: true, mode: 'assistant_raw', durationMs: 4200,
    audioPath: null, isFinal: true, errorCode: 0, errorMessage: null, sourceDeviceId: null,
    sourceDeviceName: null, asrModelName: 'Fixture ASR', llmModelName: null, editedText: r.editedText
  }));
  if (scenario !== 'many') history.push({ ...history[0] ?? {}, id: 'plain', timestamp: new Date(now - 7200000).toISOString(),
    text: '明天下午三点开会，记得带上季度报表。', mode: 'assistant_raw', durationMs: 3100, errorCode: 0, editedText: null,
    audioPath: null, sourceDeviceId: null, sourceDeviceName: null, originalText: null, asrModelName: 'Fixture ASR', llmModelName: null });
  const emit = () => {
    for (const [id, listener] of listeners) if (listener.event === 'post-edit:updated' || listener.event === 'history:updated') {
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
      if (command === 'delete_post_edit_record') {
        records = records.filter(r => r.id !== args.id);
        const row = history.find(h => h.id === args.recordId);
        if (row) row.editedText = null;
        emit();
        return;
      }
      if (command === 'set_history_final_text') {
        const row = history.find(h => h.id === args.recordId);
        const pair = records.find(r => r.recordId === args.recordId);
        if (row) row.editedText = args.text === row.text ? null : args.text;
        const original = pair ? pair.originalText : row?.text;
        if (args.text === original) records = records.filter(r => r !== pair);
        else if (pair) Object.assign(pair, { editedText: args.text, source: 'manual' });
        else if (mode === 'records' && row) records.unshift({ ...(records[0] ?? {}), id: 'manual-' + row.id, recordId: row.id,
          timestamp: row.timestamp, lastObservedAt: new Date().toISOString(), appName: null, originalText: row.text, editedText: args.text, source: 'manual' });
        emit();
        return { editedText: row?.editedText ?? null };
      }
      if (command === 'get_history') return history.slice(args.offset, args.offset + args.limit);
      if (command === 'get_settings') return { uiLanguage: 'zh-CN' };
      if (command === 'get_resolved_ui_locale') return 'zh-CN';
      if (command === 'get_input_devices' || command === 'get_recent_target_apps') return [];
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
