import { defineStore, acceptHMRUpdate } from 'pinia'
import { ref, watch } from 'vue'
import { invoke } from '@tauri-apps/api/core'

export interface AppSettings {
    // ASR
    asrProviderType: 'volcengine' | 'google' | 'qwen' | 'coli'
    asrAppKey: string
    asrAccessKey: string
    asrResourceId: string
    asrWsUrl: string
    enableNonstream: boolean
    endWindowSize: number | null
    forceToSpeechTime: number | null
    enableDdc: boolean
    coliCommandPath: string
    coliUseVad: boolean
    coliAsrIntervalMs: number
    coliFinalRefinementMode: 'off' | 'sensevoice' | 'whisper'

    // ASR Provider: Google Cloud Speech-to-Text V2
    googleSttApiKey: string
    googleSttProjectId: string
    googleSttLanguageCode: string
    googleSttLocation: string
    googleSttEndpointing: 'supershort' | 'short' | 'standard'
    googleSttPhraseBoost: number

    // ASR Provider: Qwen Realtime ASR
    qwenAsrApiKey: string
    qwenAsrModel: string
    qwenAsrWsUrl: string
    qwenAsrLanguage: string

    // LLM
    enableLlmCorrection: boolean
    llmProviderType: 'volcengine' | 'openai' | 'qwen' | 'custom'
    llmPromptTemplate: string
    translationPromptTemplate: string
    enableLlmHistoryContext: boolean
    translationEnabled: boolean
    translationTriggerMode: 'double_tap' | 'off'
    translationTargetLanguage: string
    doubleTapWindowMs: number

    // LLM Provider: Volcengine
    llmVolcengineBaseUrl: string
    llmVolcengineApiKey: string
    llmVolcengineModel: string
    llmVolcengineReasoningEffort: string | null

    // LLM Provider: OpenAI
    llmOpenaiBaseUrl: string
    llmOpenaiApiKey: string
    llmOpenaiModel: string

    // LLM Provider: Qwen (DashScope)
    llmQwenBaseUrl: string
    llmQwenApiKey: string
    llmQwenModel: string

    // LLM Provider: Custom
    llmCustomBaseUrl: string
    llmCustomApiKey: string
    llmCustomModel: string

    // Hotkey
    hotkeyConfig: string | null
    holdThresholdMs: number
    maxRecordingMinutes: number

    // Input
    inputDeviceUid: string | null
    textInjectionMode: 'pasteboard' | 'typing'

    // Sync
    syncEnabled: boolean
    syncServerUrl: string
    syncToken: string
    syncSharedSecret: string
    syncDeviceName: string

    // Retention
    audioRetentionDays: number
    textRetentionDays: number

    // Dictionary
    dictionaryText: string

    // Post-Processing
    removeTrailingPunctuation: boolean
    shortSentenceThreshold: number
    replacementRules: Array<{
        id: string
        keyword: string
        replacement: string
        matchMode: 'exact' | 'contains' | 'regex'
        enabled: boolean
    }>

    // Online Hotwords
    volcAccessKey: string
    volcSecretKey: string
    volcAppId: string
    onlineHotwordId: string
    remoteHotwordUpdatedAt: string
    localHotwordUpdatedAt: string

    // Diagnostics
    enableDiagnostics: boolean
}

export interface HotwordSyncResult {
    status: string
    message: string
    remote_updated_at: string
    local_updated_at: string
    diagnostics?: HotwordSyncDiagnostics
}

export interface HotwordSyncDiagnostics {
    server_updated_at: string
    remote_synced_at: string
    local_updated_at: string
    local_word_count: number
    remote_word_count: number
    server_newer: boolean
    local_newer: boolean
    count_mismatch: boolean
    has_file_content: boolean
    file_size: number
    linked_table_id: string
    table_name: string
}

const DEFAULT_LLM_PROMPT = `你是一个语音转写文本纠正助手。

你的任务：
- 修正语音识别文本中的识别错误、同音字错误、错别字和标点问题
- 保持原意，不增删信息
- 当识别结果中出现与用户词典中词汇发音相似、拼写接近或语义相关的词时，将其替换为词典中的标准形式
- 不要更改词典中词汇的拼写、大小写或符号
- 即便识别文本中的英文和用户词典的词汇语义相似，不要用用户词典中的词汇去替换原文中的英文

用户热词词典：
{{DICTIONARY}}

输出：
纠正后的文本或原文（如果不需要任何修改），另外不要输出任何其他说明性的内容`

const DEFAULT_TRANSLATION_PROMPT = `你是一个专业翻译助手。

你的任务：
- 将用户提供的原文准确翻译成英文
- 保持原意，不增删信息
- 保留专有名词、数字、代码片段与格式
- 如果原文已经是英文，只做必要润色并保持原意

输出：
只输出英文结果，不要输出解释或额外说明`

const defaultSettings: AppSettings = {
    asrProviderType: 'volcengine',
    asrAppKey: '',
    asrAccessKey: '',
    asrResourceId: 'volc.seedasr.sauc.duration',
    asrWsUrl: 'wss://openspeech.bytedance.com/api/v3/sauc/bigmodel_async',
    enableNonstream: false,
    endWindowSize: 1400,
    forceToSpeechTime: 3500,
    enableDdc: true,
    coliCommandPath: '',
    coliUseVad: false,
    coliAsrIntervalMs: 1000,
    coliFinalRefinementMode: 'off',

    googleSttApiKey: '',
    googleSttProjectId: '',
    googleSttLanguageCode: 'cmn-Hans-CN, en-US',
    googleSttLocation: 'us',
    googleSttEndpointing: 'supershort',
    googleSttPhraseBoost: 8,

    qwenAsrApiKey: '',
    qwenAsrModel: 'qwen3-asr-flash-realtime',
    qwenAsrWsUrl: 'wss://dashscope.aliyuncs.com/api-ws/v1/realtime',
    qwenAsrLanguage: 'zh',

    enableLlmCorrection: false,
    llmProviderType: 'volcengine',
    llmPromptTemplate: DEFAULT_LLM_PROMPT,
    enableLlmHistoryContext: false,
    translationPromptTemplate: DEFAULT_TRANSLATION_PROMPT,
    translationEnabled: true,
    translationTriggerMode: 'double_tap',
    translationTargetLanguage: 'en',
    doubleTapWindowMs: 400,

    llmVolcengineBaseUrl: 'https://ark.cn-beijing.volces.com/api/v3',
    llmVolcengineApiKey: '',
    llmVolcengineModel: 'doubao-seed-2-0-mini-260215',
    llmVolcengineReasoningEffort: 'minimal',

    llmOpenaiBaseUrl: 'https://api.openai.com/v1',
    llmOpenaiApiKey: '',
    llmOpenaiModel: 'gpt-4o-mini',

    llmQwenBaseUrl: 'https://dashscope.aliyuncs.com/compatible-mode/v1',
    llmQwenApiKey: '',
    llmQwenModel: 'qwen3.5-flash',

    llmCustomBaseUrl: '',
    llmCustomApiKey: '',
    llmCustomModel: '',

    hotkeyConfig: null,
    holdThresholdMs: 1000,
    maxRecordingMinutes: 5,

    inputDeviceUid: null,
    textInjectionMode: 'pasteboard',

    syncEnabled: false,
    syncServerUrl: '',
    syncToken: '',
    syncSharedSecret: '',
    syncDeviceName: '',

    audioRetentionDays: 7,
    textRetentionDays: 30,

    dictionaryText: '',

    removeTrailingPunctuation: true,
    shortSentenceThreshold: 5,
    replacementRules: [],

    volcAccessKey: '',
    volcSecretKey: '',
    volcAppId: '',
    onlineHotwordId: '',
    remoteHotwordUpdatedAt: '',
    localHotwordUpdatedAt: '',

    enableDiagnostics: false
}

export const useSettingsStore = defineStore('settings', () => {
    const settings = ref<AppSettings>({ ...defaultSettings })
    const isLoading = ref(true)
    const saveTimeout = ref<number | null>(null)
    const applyTimeout = ref<number | null>(null)
    const hotwordSyncInProgress = ref(false)
    const hotwordSyncTimer = ref<number | null>(null)
    const HOTWORD_SYNC_INTERVAL_MS = 5 * 60 * 1000
    const lastHotwordSyncResult = ref<HotwordSyncResult | null>(null)

    // Load settings from backend
    async function loadSettings() {
        try {
            isLoading.value = true
            const result = await invoke<AppSettings>('get_settings')
            Object.assign(settings.value, result)
            await applyHotkeyConfig()
        } catch (error) {
            console.error('Failed to load settings:', error)
        } finally {
            isLoading.value = false
        }
    }

    // Save settings to backend (debounced)
    async function saveSettings() {
        try {
            await invoke('save_settings', { settings: settings.value })
        } catch (error) {
            console.error('Failed to save settings:', error)
        }
    }

    // Force save settings immediately (cancel debounce if any)
    async function forceSaveSettings() {
        if (saveTimeout.value !== null) {
            clearTimeout(saveTimeout.value)
            saveTimeout.value = null
        }
        await saveSettings()
    }

    // Debounced save - auto-save after 500ms of no changes
    function debouncedSave() {
        if (saveTimeout.value !== null) {
            clearTimeout(saveTimeout.value)
        }
        saveTimeout.value = window.setTimeout(() => {
            saveSettings()
            saveTimeout.value = null
        }, 500)
    }

    // Watch for changes and auto-save
    watch(settings, () => {
        if (!isLoading.value) {
            debouncedSave()
        }
    }, { deep: true })

    // Update a single setting
    function updateSetting<K extends keyof AppSettings>(key: K, value: AppSettings[K]) {
        settings.value[key] = value
    }

    function hasHotwordCredentials() {
        return Boolean(settings.value.volcAccessKey && settings.value.volcSecretKey && settings.value.volcAppId)
    }

    async function syncHotwords(options: { reason?: string; silent?: boolean } = {}) {
        if (hotwordSyncInProgress.value) return null
        if (!hasHotwordCredentials()) {
            if (options.silent) return null
            throw new Error('Please configure Volcengine AK, SK, and App ID first.')
        }

        hotwordSyncInProgress.value = true
        try {
            const result = await invoke<HotwordSyncResult>('sync_hotwords')
            lastHotwordSyncResult.value = result ?? null
            // Only reload settings on download/create — these bring remote content into local.
            // After an upload, the backend already has what we sent; reloading would overwrite
            // any text the user is currently editing in the textarea.
            if (result?.status === 'downloaded' || result?.status === 'created') {
                await loadSettings()
            }
            return result
        } catch (error) {
            if (options.silent) {
                console.error('Hotwords sync failed:', options.reason || 'unknown', error)
                return null
            }
            throw error
        } finally {
            hotwordSyncInProgress.value = false
        }
    }

    async function forceDownloadHotwords(options: { silent?: boolean } = {}) {
        if (hotwordSyncInProgress.value) return null
        if (!hasHotwordCredentials()) {
            if (options.silent) return null
            throw new Error('Please configure Volcengine AK, SK, and App ID first.')
        }

        hotwordSyncInProgress.value = true
        try {
            const result = await invoke<HotwordSyncResult>('force_download_hotwords')
            lastHotwordSyncResult.value = result ?? null
            if (result) {
                await loadSettings()
            }
            return result
        } catch (error) {
            if (options.silent) {
                console.error('Force download failed:', error)
                return null
            }
            throw error
        } finally {
            hotwordSyncInProgress.value = false
        }
    }

    function startHotwordSyncScheduler() {
        if (hotwordSyncTimer.value !== null) return
        hotwordSyncTimer.value = window.setInterval(() => {
            syncHotwords({ reason: 'interval', silent: true })
        }, HOTWORD_SYNC_INTERVAL_MS)
        syncHotwords({ reason: 'startup', silent: true })
    }

    function stopHotwordSyncScheduler() {
        if (hotwordSyncTimer.value === null) return
        clearInterval(hotwordSyncTimer.value)
        hotwordSyncTimer.value = null
    }

    async function applyHotkeyConfig() {
        try {
            await invoke('apply_hotkey_config', { config: settings.value.hotkeyConfig })
        } catch (error) {
            console.error('Failed to apply hotkey config', error)
        }
    }

    // Watch hotkey changes and apply debounced
    watch(
        () => settings.value.hotkeyConfig,
        () => {
            if (applyTimeout.value !== null) {
                clearTimeout(applyTimeout.value)
            }
            applyTimeout.value = window.setTimeout(() => {
                applyHotkeyConfig()
                applyTimeout.value = null
            }, 300)
        }
    )

    return {
        settings,
        isLoading,
        loadSettings,
        saveSettings,
        forceSaveSettings,
        updateSetting,
        syncHotwords,
        forceDownloadHotwords,
        lastHotwordSyncResult,
        startHotwordSyncScheduler,
        stopHotwordSyncScheduler
    }
})

if (import.meta.hot) {
    import.meta.hot.accept(acceptHMRUpdate(useSettingsStore, import.meta.hot))
}
