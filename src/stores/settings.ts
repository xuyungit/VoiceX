import { ASR_MODEL_DEFAULTS } from '../utils/asrModels'
import { defineStore, acceptHMRUpdate } from 'pinia'
import { ref, watch } from 'vue'
import { invoke } from '@tauri-apps/api/core'
import type { UiLanguage } from '../i18n'
import { getDefaultPrompt } from '../utils/llmPrompts'
import type { LlmApiModeValue, LlmProviderValue } from '../utils/llmOptions'

export interface CustomLlmEndpoint {
    id: string
    name: string
    baseUrl: string
    apiKey: string
    model: string
    apiMode: LlmApiModeValue
}

export interface AppSettings {
    uiLanguage: UiLanguage

    // ASR
    asrProviderType: 'volcengine' | 'google' | 'funasr' | 'qwen' | 'gemini' | 'gemini-live' | 'cohere' | 'openai' | 'elevenlabs' | 'soniox' | 'stepaudio' | 'mimo' | 'coli' | 'qwen-local'
    asrAppKey: string
    asrAccessKey: string
    asrResourceId: string
    asrWsUrl: string
    enableNonstream: boolean
    endWindowSize: number | null
    forceToSpeechTime: number | null
    enableDdc: boolean
    enableAsrContext: boolean
    qwenLocalCommandPath: string
    qwenLocalModelDir: string
    qwenLocalLanguage: string
    qwenLocalUseDictionary: boolean
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
    qwenAsrWorkspaceId: string
    qwenAsrLanguage: string
    qwenAsrPostRecordingRefine: boolean
    qwenAsrVocabularyId: string
    qwenAsrHotwordWeight: number
    qwenAsrSemanticPunctuationEnabled: boolean
    qwenAsrMaxSentenceSilenceMs: number
    qwenAsrHeartbeat: boolean
    geminiApiKey: string
    geminiModel: string
    geminiLiveModel: string
    geminiLanguage: 'auto' | 'zh' | 'en' | 'zh-en'
    cohereApiKey: string
    cohereModel: string
    cohereLanguage: string

    openaiAsrApiKey: string
    openaiAsrRefineModel: string
    openaiAsrModel: string
    openaiAsrBaseUrl: string
    openaiAsrLanguage: string
    openaiAsrPrompt: string
    openaiAsrMode: 'batch' | 'realtime'
    openaiAsrDelay: '' | 'minimal' | 'low' | 'medium' | 'high' | 'xhigh'
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

    // ASR Provider: StepAudio
    stepaudioApiKey: string
    stepaudioModel: string
    stepaudioBaseUrl: string
    stepaudioLanguage: 'auto' | 'zh' | 'en' | ''

    // ASR Provider: Xiaomi MiMo
    mimoApiKey: string
    mimoModel: string
    mimoBaseUrl: string
    mimoLanguage: 'auto' | 'zh' | 'en' | ''

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

    // LLM Provider: Gemini (Google Generative AI)
    llmGeminiBaseUrl: string
    llmGeminiApiKey: string
    llmGeminiModel: string

    // LLM Provider: Custom (multiple named OpenAI-compatible endpoints)
    llmCustomEndpoints: CustomLlmEndpoint[]
    llmActiveCustomEndpointId: string

    // Hotkey
    hotkeyConfig: string | null
    holdThresholdMs: number
    maxRecordingMinutes: number

    // Selected-text reading (TTS) — feature-level settings only. Every
    // synthesis parameter belongs to a provider block below, rate and volume
    // included: engines differ in baseline speed and loudness.
    ttsEnabled: boolean
    ttsProviderType: 'system' | 'volcengine' | 'aliyun' | 'mimo' | 'azure'
    ttsHotkeyConfig: string | null
    ttsClipboardFallback: boolean

    // TTS Provider: macOS system voice
    systemTtsVoiceId: string
    /** Normalized 0..1. The UI shows it as a 0.5x–2x multiplier of 0.5. */
    systemTtsRate: number
    systemTtsVolume: number
    /** Engine scale 0.5..2.0, where 1.0 is neutral. No cloud engine has one. */
    systemTtsPitch: number

    // TTS Provider: Volcengine (Doubao Seed-TTS 2.0)
    volcTtsApiKey: string
    /** The model string, not the console's instance id. */
    volcTtsResourceId: string
    volcTtsSpeaker: string
    volcTtsRate: number
    volcTtsVolume: number

    // TTS Provider: Alibaba Cloud Model Studio (百炼)
    aliyunTtsApiKey: string
    /** The models are separate services; the backend maps between them. */
    aliyunTtsModel: 'qwen3-tts-flash' | 'qwen-audio-3.0-tts-flash' | 'cosyvoice-v3-flash' | 'cosyvoice-v3.5-flash'
    /** One voice per model family — each rejects the others' ids outright. */
    aliyunTtsVoiceQwen3: string
    aliyunTtsVoiceQwenAudio: string
    aliyunTtsVoiceCosyVoice: string
    aliyunTtsVoiceCosyVoiceV35: string
    aliyunTtsRate: number
    aliyunTtsVolume: number

    // TTS Provider: Xiaomi MiMo
    mimoTtsApiKey: string
    mimoTtsVoice: string
    /** Natural-language style instruction — MiMo's only delivery control;
     * the API has no speed or pitch parameter. Empty means none. */
    mimoTtsInstruction: string
    /** Local playback gain; the API has no volume parameter either. */
    mimoTtsVolume: number

    // TTS Provider: Azure Speech (Microsoft Cognitive Services)
    azureTtsApiKey: string
    /** Azure region short name (eastus, eastasia, …) — keys are per region. */
    azureTtsRegion: string
    azureTtsVoice: string
    azureTtsRate: number
    /** Local playback gain, like the other cloud providers. */
    azureTtsVolume: number

    // Input
    inputDeviceUid: string | null
    textInjectionMode: 'pasteboard' | 'typing'
    textInjectionOverrides: Array<{
        platform: string
        appName: string
        matchKind: string
        matchValue: string
        mode: 'pasteboard' | 'typing'
        skipClipboardRestore?: boolean
    }>
    hudTransparent: boolean

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
    enableAsrContext: false,
    qwenLocalCommandPath: '',
    qwenLocalModelDir: '',
    qwenLocalLanguage: 'Chinese',
    qwenLocalUseDictionary: true,
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
    funasrModel: ASR_MODEL_DEFAULTS.funasr,
    funasrWsUrl: 'wss://dashscope.aliyuncs.com/api-ws/v1/inference',
    funasrLanguage: '',

    qwenAsrApiKey: '',
    qwenAsrRecognitionMode: 'realtime',
    qwenAsrModel: ASR_MODEL_DEFAULTS.qwen,
    qwenAsrBatchModel: ASR_MODEL_DEFAULTS.qwenBatch,
    qwenAsrWsUrl: 'wss://dashscope.aliyuncs.com/api-ws/v1/realtime',
    qwenAsrWorkspaceId: '',
    qwenAsrLanguage: '',
    qwenAsrPostRecordingRefine: false,
    qwenAsrVocabularyId: '',
    qwenAsrHotwordWeight: 4,
    qwenAsrSemanticPunctuationEnabled: false,
    qwenAsrMaxSentenceSilenceMs: 1300,
    qwenAsrHeartbeat: false,
    geminiApiKey: '',
    geminiModel: ASR_MODEL_DEFAULTS.gemini,
    geminiLiveModel: ASR_MODEL_DEFAULTS.geminiLive,
    geminiLanguage: 'auto',
    cohereApiKey: '',
    cohereModel: ASR_MODEL_DEFAULTS.cohere,
    cohereLanguage: 'zh',
    openaiAsrApiKey: '',
    openaiAsrModel: ASR_MODEL_DEFAULTS.openai,
    openaiAsrRefineModel: ASR_MODEL_DEFAULTS.openaiRefine,
    openaiAsrBaseUrl: 'https://api.openai.com/v1',
    openaiAsrLanguage: '',
    openaiAsrPrompt: 'Transcribe faithfully with natural punctuation and capitalization. Preserve the original wording and do not omit spoken content.',
    openaiAsrMode: 'batch',
    openaiAsrDelay: '',
    openaiAsrPostRecordingRefine: 'off',
    elevenlabsApiKey: '',
    elevenlabsRecognitionMode: 'realtime',
    elevenlabsPostRecordingRefine: 'off',
    elevenlabsRealtimeModel: ASR_MODEL_DEFAULTS.elevenlabsRealtime,
    elevenlabsBatchModel: ASR_MODEL_DEFAULTS.elevenlabsBatch,
    elevenlabsLanguage: '',
    elevenlabsEnableKeyterms: true,
    sonioxApiKey: '',
    sonioxModel: ASR_MODEL_DEFAULTS.soniox,
    sonioxLanguage: '',
    sonioxMaxEndpointDelayMs: null,
    stepaudioApiKey: '',
    stepaudioModel: ASR_MODEL_DEFAULTS.stepaudio,
    stepaudioBaseUrl: 'https://api.stepfun.com/v1',
    stepaudioLanguage: 'auto',
    mimoApiKey: '',
    mimoModel: ASR_MODEL_DEFAULTS.mimo,
    mimoBaseUrl: 'https://api.xiaomimimo.com/v1',
    mimoLanguage: 'auto',

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

    llmGeminiBaseUrl: 'https://generativelanguage.googleapis.com',
    llmGeminiApiKey: '',
    llmGeminiModel: 'gemini-3.5-flash-lite',

    llmCustomEndpoints: [],
    llmActiveCustomEndpointId: '',

    hotkeyConfig: null,
    holdThresholdMs: 1000,
    maxRecordingMinutes: 5,

    ttsEnabled: true,
    ttsProviderType: 'system',
    ttsHotkeyConfig: null,
    ttsClipboardFallback: true,

    systemTtsVoiceId: '',
    systemTtsRate: 0.5,
    systemTtsVolume: 1,
    systemTtsPitch: 1,

    volcTtsApiKey: '',
    volcTtsResourceId: 'seed-tts-2.0',
    volcTtsSpeaker: 'zh_female_vv_uranus_bigtts',
    volcTtsRate: 0.5,
    volcTtsVolume: 1,

    aliyunTtsApiKey: '',
    aliyunTtsModel: 'qwen-audio-3.0-tts-flash',
    aliyunTtsVoiceQwen3: 'Cherry',
    aliyunTtsVoiceQwenAudio: 'longanfengyue',
    aliyunTtsVoiceCosyVoice: 'longanyang',
    aliyunTtsVoiceCosyVoiceV35: '',
    aliyunTtsRate: 0.5,
    aliyunTtsVolume: 1,

    mimoTtsApiKey: '',
    mimoTtsVoice: 'mimo_default',
    mimoTtsInstruction: '',
    mimoTtsVolume: 1,

    azureTtsApiKey: '',
    azureTtsRegion: 'eastus',
    azureTtsVoice: 'zh-CN-XiaoyuMultilingualNeural',
    azureTtsRate: 0.5,
    azureTtsVolume: 1,

    inputDeviceUid: null,
    textInjectionMode: 'pasteboard',
    textInjectionOverrides: [],
    hudTransparent: false,

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
