// Isolated rendering of the real frontend: no native backend or personal history.
import { createServer } from 'vite'
import { readFileSync } from 'node:fs'

const usage = JSON.parse(readFileSync(new URL('./usage-snapshot.json', import.meta.url), 'utf8'))

function installFixture(usage) {
  const params = new URLSearchParams(location.search)
  const locale = params.get('lang') === 'en' ? 'en-US' : 'zh-CN'
  const english = locale === 'en-US'
  let settings = {
    ...usage.settings,
    enableLlmCorrection: Boolean(usage.settings.enableLlmCorrection),
    syncEnabled: Boolean(usage.settings.syncEnabled),
    uiLanguage: locale,
    uiTheme: params.get('theme') === 'dark' ? 'dark' : 'light',
    ...(location.pathname === '/asr-settings' ? { asrProviderType: 'soniox' } : {}),
    sonioxLanguage: 'zh, en',
    ttsProviderType: 'edge',
    edgeTtsVoice: english ? 'en-US-AriaNeural' : 'zh-CN-XiaoxiaoNeural',
    ttsTranslateVoiceOverrides: { edge: 'en-US-AriaNeural' },
  }
  const texts = english ? [
    'The new interface brings dictation, translation, and reading into one workspace.',
    'Please review the release notes and update the screenshots before Friday.',
    'A clear layout helps you focus on your words. Use the global shortcut to start dictating in any app.',
    'Let’s meet tomorrow at three to discuss the next version.',
    'VoiceX supports multiple speech recognition engines and a shared dictionary for technical terms.',
  ] : [
    '新版界面把听写、翻译和朗读整合在同一个工作台里。',
    '请检查这一版的更新说明，并在周五之前更新软件截图。',
    '清晰的界面让你专注于表达。在任意应用里按下全局热键，就可以开始听写。',
    '我们明天下午三点开会，讨论下一个版本的计划。',
    'VoiceX 支持多种语音识别引擎，也可以通过共享词典提高专业术语的识别效果。',
  ]
  const history = texts.map((text, i) => ({
    id: `demo-${i}`, timestamp: `2026-11-0${i < 3 ? 5 : 4}T${['10:24', '09:51', '09:18', '16:32', '14:05'][i]}:00+08:00`,
    text, originalText: i === 1 ? text.replace(english ? 'Friday' : '周五', english ? 'friday' : '周5') : null,
    aiCorrectionApplied: i === 1, llmInvoked: i === 1, mode: 'assistant_raw',
    durationMs: [8200, 6400, 14200, 5100, 12300][i], audioPath: null,
    isFinal: true, errorCode: 0, errorMessage: null, sourceDeviceId: 'demo',
    sourceDeviceName: i === 4 ? 'Windows PC' : 'MacBook Pro',
    asrModelName: 'Soniox · stt-rt-v6', llmModelName: i === 1 ? 'Demo LLM' : null,
    editedText: i === 0 ? text + (english ? ' Everything is a shortcut away.' : '常用操作都可以通过快捷键完成。') : null,
  }))
  let nextId = 0
  const callbacks = new Map()
  const listeners = new Map()
  const emit = (event, payload) => {
    for (const [id, listener] of listeners) {
      if (listener.event === event) callbacks.get(listener.handler)?.({ event, id, payload })
    }
  }
  window.__TAURI_EVENT_PLUGIN_INTERNALS__ = { unregisterListener() {} }
  window.__TAURI_INTERNALS__ = {
    metadata: { currentWindow: { label: 'main' }, currentWebview: { label: 'main' } },
    transformCallback(callback) { const id = ++nextId; callbacks.set(id, callback); return id },
    unregisterCallback(id) { callbacks.delete(id) },
    async invoke(command, args = {}) {
      switch (command) {
        case 'get_settings': {
          const { getDefaultPrompt } = await import('/src/utils/llmPrompts.ts')
          return { ...structuredClone(settings), ttsTranslatePromptTemplate: getDefaultPrompt('ttsTranslate', locale) }
        }
        // Match Tauri's JSON serialization; Vue's reactive proxy cannot be structured-cloned.
        case 'save_settings': settings = JSON.parse(JSON.stringify(args.settings)); return
        case 'get_resolved_ui_locale': return locale
        // Every permission granted unless `perm=missing` asks for the
        // first-launch state the Overview setup card and guide are shown in.
        case 'permissions_status': return params.get('perm') === 'missing'
          ? { platform: 'macos', microphone: 'notDetermined', accessibility: 'denied', inputMonitoring: 'denied', hotkeyListenerActive: false }
          : { platform: 'macos', microphone: 'granted', accessibility: 'granted', inputMonitoring: 'granted', hotkeyListenerActive: true }
        case 'permission_guide_info':
          return { kind: params.get('kind') || 'accessibility', appPath: '/Applications/VoiceX.app', isAppBundle: true }
        case 'get_usage_stats': return structuredClone(usage.total)
        case 'get_local_usage_stats': return structuredClone(usage.local)
        case 'get_history': return structuredClone(history.slice(args.offset, args.offset + args.limit))
        case 'get_post_edit_settings': return { mode: 'off' }
        case 'get_sync_state': return { state: { status: 'live' }, deviceId: 'demo' }
        case 'get_input_devices': return [{ name: 'MacBook Pro Microphone', uid: 'demo-mic', isDefault: true }]
        case 'get_recent_target_apps': return []
        case 'get_output_mute_status': return { revision: 0, recording: false, support: 'supported', pendingRestores: [], recoveryError: null }
        case 'get_post_edit_test_status': return { enabled: false, status: 'disabled', reason: 'none', nativeCode: null }
        case 'read_selection_hotkey_status':
        case 'translate_selection_hotkey_status':
        case 'apply_translate_selection_hotkey': return { bound: true, enabled: true, conflictsWithDictation: false, conflictsWithReading: false, display: null }
        case 'list_tts_voices': return { customVoiceOnly: false, voices: [
          { id: 'zh-CN-XiaoxiaoNeural', name: 'Xiaoxiao', language: 'zh-CN' },
          { id: 'en-US-AriaNeural', name: 'Aria', language: 'en-US' },
        ] }
        case 'plugin:event|listen': { const id = ++nextId; listeners.set(id, args); return id }
        case 'plugin:event|unlisten': listeners.delete(args.eventId); return
        case 'apply_hotkey_config':
        case 'set_input_device':
        case 'plugin:window|set_theme':
        case 'set_hud_content_bounds': window.__readmeHudBounds = args; return
        case 'hud_ready':
          if (params.get('hud') === 'reading') {
            emit('state:hud_presentation', { mode: 'caption' })
            emit('state:reading', { phase: 'speaking', kind: 'read', source: 'selection' })
            emit('state:caption', { text: english
              ? 'Select text, press the reading shortcut, and follow along with captions.'
              : '选中一段文字，按下朗读热键，就能一边听、一边看字幕。' })
            return
          }
          emit('state:recording_style', { style: 'hands_free' })
          emit('asr:event', { text: english ? 'Turn your ideas into words with VoiceX.' : '用 VoiceX 说出想法，让语音变成文字。', isFinal: false })
          return
        default: throw new Error(`Screenshot fixture does not implement: ${command}`)
      }
    },
  }
}

const server = await createServer({
  server: { host: '127.0.0.1', port: 1523, strictPort: true, open: false },
  plugins: [{
    name: 'readme-screenshot-fixture',
    transformIndexHtml: { order: 'pre', handler: () => [{
      tag: 'script', injectTo: 'head-prepend',
      children: `(${installFixture.toString()})(${JSON.stringify(usage)})`,
    }] },
  }],
})
await server.listen()
console.log('README preview: http://127.0.0.1:1523/overview?lang=zh (lang=en, theme=dark)')
for (const signal of ['SIGINT', 'SIGTERM']) process.on(signal, async () => { await server.close(); process.exit() })
