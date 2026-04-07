import { defineStore, acceptHMRUpdate } from 'pinia'
import { ref, watch } from 'vue'
import { invoke } from '@tauri-apps/api/core'
import type { UiLanguage } from '../i18n'
import { getDefaultPrompt } from '../utils/llmPrompts'
import type { LlmApiModeValue, LlmProviderValue } from '../utils/llmOptions'

export interface AppSettings {
    uiLanguage: UiLanguage

    // ASR
    asrProviderType: 'volcengine' | 'google' | 'funasr' | 'qwen' | 'gemini' | 'gemini-live' | 'cohere' | 'openai' | 'elevenlabs' | 'soniox' | 'coli'
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
    coliRealtime: boolean

    // ASR Provider: Google Cloud Speech-to-Text V2
    googleSttApiKey: string
    googleSttProjectId: string
    googleSttLanguageCode: string
    googleSttLocation: string
    googleSttEndpointing: 'supershort' | 'short' | 'standard'
    googleSttPhraseBoost: number

    // ASR Provider: DashScope Fun-ASR realtime
    funasrApiKey: string
    funasrModel: string
    funasrWsUrl: string
    funasrLanguage: string

    // ASR Provider: Qwen Realtime ASR
    qwenAsrApiKey: string
    qwenAsrRecognitionMode: 'realtime' | 'batch'
    qwenAsrModel: string
    qwenAsrBatchModel: string
    qwenAsrWsUrl: string
    qwenAsrLanguage: string
    qwenAsrPostRecordingRefine: boolean
    geminiApiKey: string
    geminiModel: string
    geminiLiveModel: string
    geminiLanguage: 'auto' | 'zh' | 'en' | 'zh-en'
    cohereApiKey: string
    cohereModel: string
    cohereLanguage: string

    openaiAsrApiKey: string
    openaiAsrModel: string
    openaiAsrBaseUrl: string
    openaiAsrLanguage: string
    openaiAsrPrompt: string
    openaiAsrMode: 'batch' | 'realtime'
    openaiAsrPostRecordingRefine: 'off' | 'batch_refine'

    // ASR Provider: ElevenLabs
    elevenlabsApiKey: string
    elevenlabsRecognitionMode: 'realtime' | 'batch'
    elevenlabsPostRecordingRefine: 'off' | 'batch_refine'
    elevenlabsRealtimeModel: string
    elevenlabsBatchModel: string
    elevenlabsLanguage: string
    elevenlabsEnableKeyterms: boolean

    // ASR Provider: Soniox
    sonioxApiKey: string
    sonioxModel: string
    sonioxLanguage: string
    sonioxMaxEndpointDelayMs: number | null

    // LLM
    enableLlmCorrection: boolean
    llmProviderType: LlmProviderValue
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
    llmCustomApiMode: LlmApiModeValue

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

const defaultSettings: AppSettings = {
    uiLanguage: 'system',
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
    coliUseVad: true,
    coliAsrIntervalMs: 1000,
    coliFinalRefinementMode: 'off',
    coliRealtime: true,

    googleSttApiKey: '',
    googleSttProjectId: '',
    googleSttLanguageCode: 'cmn-Hans-CN, en-US',
    googleSttLocation: 'us',
    googleSttEndpointing: 'supershort',
    googleSttPhraseBoost: 8,

    funasrApiKey: '',
    funasrModel: 'fun-asr-realtime',
    funasrWsUrl: 'wss://dashscope.aliyuncs.com/api-ws/v1/inference',
    funasrLanguage: '',

    qwenAsrApiKey: '',
    qwenAsrRecognitionMode: 'realtime',
    qwenAsrModel: 'qwen3-asr-flash-realtime',
    qwenAsrBatchModel: 'qwen3-asr-flash',
    qwenAsrWsUrl: 'wss://dashscope.aliyuncs.com/api-ws/v1/realtime',
    qwenAsrLanguage: '',
    qwenAsrPostRecordingRefine: false,
    geminiApiKey: '',
    geminiModel: 'gemini-3.1-flash-lite-preview',
    geminiLiveModel: 'gemini-3.1-flash-live-preview',
    geminiLanguage: 'auto',
    cohereApiKey: '',
    cohereModel: 'cohere-transcribe-03-2026',
    cohereLanguage: 'zh',
    openaiAsrApiKey: '',
    openaiAsrModel: 'gpt-4o-transcribe',
    openaiAsrBaseUrl: 'https://api.openai.com/v1',
    openaiAsrLanguage: '',
    openaiAsrPrompt: 'Transcribe faithfully with natural punctuation and capitalization. Preserve the original wording and do not omit spoken content.',
    openaiAsrMode: 'batch',
    openaiAsrPostRecordingRefine: 'off',
    elevenlabsApiKey: '',
    elevenlabsRecognitionMode: 'realtime',
    elevenlabsPostRecordingRefine: 'off',
    elevenlabsRealtimeModel: 'scribe_v2_realtime',
    elevenlabsBatchModel: 'scribe_v2',
    elevenlabsLanguage: '',
    elevenlabsEnableKeyterms: true,
    sonioxApiKey: '',
    sonioxModel: 'stt-rt-v4',
    sonioxLanguage: '',
    sonioxMaxEndpointDelayMs: null,

    enableLlmCorrection: false,
    llmProviderType: 'volcengine',
    llmPromptTemplate: getDefaultPrompt('assistant', 'zh-CN'),
    enableLlmHistoryContext: false,
    translationPromptTemplate: getDefaultPrompt('translation', 'zh-CN'),
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
    llmCustomApiMode: 'chat_completions',

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

    watch(
        () => settings.value.enableDiagnostics,
        (enabled, previous) => {
            if (isLoading.value || enabled || previous === undefined) {
                return
            }
            invoke('clear_soniox_debug_overrides').catch((error) => {
                console.error('Failed to clear Soniox debug overrides:', error)
            })
        }
    )

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
