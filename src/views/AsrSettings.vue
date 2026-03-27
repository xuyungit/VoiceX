<script setup lang="ts">
import { computed, onMounted, ref, watch } from 'vue'
import { invoke } from '@tauri-apps/api/core'
import { NButton, NCheckbox, NInput, NInputNumber, NSelect } from 'naive-ui'
import { useI18n } from 'vue-i18n'
import { useSettingsStore } from '../stores/settings'

const settingsStore = useSettingsStore()
const { t } = useI18n()

type LocalAsrStatus = {
  available: boolean
  configuredCommand: string
  resolvedPath: string | null
  ffmpegAvailable: boolean
  modelsDir: string | null
  sensevoiceInstalled: boolean
  whisperInstalled: boolean
  vadInstalled: boolean
  message: string
}

type ProviderValue = 'volcengine' | 'google' | 'qwen' | 'gemini' | 'gemini-live' | 'cohere' | 'coli'

const asrProviderType = computed({
  get: () => settingsStore.settings.asrProviderType,
  set: (v: ProviderValue) => {
    if (v === 'coli' && coliStatus.value && !coliStatus.value.available) {
      return
    }
    settingsStore.updateSetting('asrProviderType', v)
  }
})

const isVolcengine = computed(() => settingsStore.settings.asrProviderType === 'volcengine')
const isGoogle = computed(() => settingsStore.settings.asrProviderType === 'google')
const isQwen = computed(() => settingsStore.settings.asrProviderType === 'qwen')
const isGemini = computed(() => settingsStore.settings.asrProviderType === 'gemini')
const isGeminiLive = computed(() => settingsStore.settings.asrProviderType === 'gemini-live')
const isCohere = computed(() => settingsStore.settings.asrProviderType === 'cohere')
const isColi = computed(() => settingsStore.settings.asrProviderType === 'coli')

// Volcengine settings
const asrAppKey = computed({
  get: () => settingsStore.settings.asrAppKey,
  set: (v: string) => settingsStore.updateSetting('asrAppKey', v)
})

const asrAccessKey = computed({
  get: () => settingsStore.settings.asrAccessKey,
  set: (v: string) => settingsStore.updateSetting('asrAccessKey', v)
})

const asrResourceId = computed({
  get: () => settingsStore.settings.asrResourceId,
  set: (v: string) => settingsStore.updateSetting('asrResourceId', v)
})

const asrWsUrl = computed({
  get: () => settingsStore.settings.asrWsUrl,
  set: (v: string) => settingsStore.updateSetting('asrWsUrl', v)
})

const recognitionModeOptions = computed(() => [
  { label: t('asr.recognitionModeRealtime'), value: 'realtime_async' },
  { label: t('asr.recognitionModeNostream'), value: 'nostream' }
])

const recognitionMode = computed({
  get: () => (settingsStore.settings.asrWsUrl.includes('nostream') ? 'nostream' : 'realtime_async'),
  set: (v: string) => {
    const url =
      v === 'nostream'
        ? 'wss://openspeech.bytedance.com/api/v3/sauc/bigmodel_nostream'
        : 'wss://openspeech.bytedance.com/api/v3/sauc/bigmodel_async'
    settingsStore.updateSetting('asrWsUrl', url)
  }
})

// Google settings
const googleSttApiKey = computed({
  get: () => settingsStore.settings.googleSttApiKey,
  set: (v: string) => settingsStore.updateSetting('googleSttApiKey', v)
})

const googleSttProjectId = computed({
  get: () => settingsStore.settings.googleSttProjectId,
  set: (v: string) => settingsStore.updateSetting('googleSttProjectId', v)
})

const googleSttLocation = computed({
  get: () => settingsStore.settings.googleSttLocation,
  set: (v: string) => settingsStore.updateSetting('googleSttLocation', v)
})

const googleSttLanguageCode = computed({
  get: () => settingsStore.settings.googleSttLanguageCode,
  set: (v: string) => settingsStore.updateSetting('googleSttLanguageCode', v)
})

const googleSttEndpointing = computed({
  get: () => settingsStore.settings.googleSttEndpointing,
  set: (v: 'supershort' | 'short' | 'standard') => settingsStore.updateSetting('googleSttEndpointing', v)
})

const googleSttPhraseBoost = computed({
  get: () => settingsStore.settings.googleSttPhraseBoost,
  set: (v: number | null) => settingsStore.updateSetting('googleSttPhraseBoost', v ?? 0)
})

const googleEndpointingOptions = computed(() => [
  { label: t('asr.endpointingSupershort'), value: 'supershort' },
  { label: t('asr.endpointingShort'), value: 'short' },
  { label: t('asr.endpointingStandard'), value: 'standard' },
])

const googleLocationOptions = [
  { label: 'us (Multi-region)', value: 'us' },
  { label: 'eu (Multi-region)', value: 'eu' },
  { label: 'asia-southeast1 (Singapore)', value: 'asia-southeast1' },
  { label: 'asia-northeast1 (Tokyo)', value: 'asia-northeast1' },
  { label: 'asia-south1 (Mumbai) [Preview]', value: 'asia-south1' },
  { label: 'europe-west2 (London) [Preview]', value: 'europe-west2' },
  { label: 'europe-west3 (Frankfurt) [Preview]', value: 'europe-west3' },
  { label: 'northamerica-northeast1 (Montreal) [Preview]', value: 'northamerica-northeast1' },
]

// Qwen settings
const qwenAsrApiKey = computed({
  get: () => settingsStore.settings.qwenAsrApiKey,
  set: (v: string) => settingsStore.updateSetting('qwenAsrApiKey', v)
})

const qwenAsrModel = computed({
  get: () => settingsStore.settings.qwenAsrModel,
  set: (v: string) => settingsStore.updateSetting('qwenAsrModel', v)
})

const qwenAsrWsUrl = computed({
  get: () => settingsStore.settings.qwenAsrWsUrl,
  set: (v: string) => settingsStore.updateSetting('qwenAsrWsUrl', v)
})

const qwenAsrLanguage = computed({
  get: () => settingsStore.settings.qwenAsrLanguage,
  set: (v: string) => settingsStore.updateSetting('qwenAsrLanguage', v)
})

// Gemini settings
const geminiApiKey = computed({
  get: () => settingsStore.settings.geminiApiKey,
  set: (v: string) => settingsStore.updateSetting('geminiApiKey', v)
})

const geminiModel = computed({
  get: () => settingsStore.settings.geminiModel,
  set: (v: string) => settingsStore.updateSetting('geminiModel', v)
})

const geminiLiveModel = computed({
  get: () => settingsStore.settings.geminiLiveModel,
  set: (v: string) => settingsStore.updateSetting('geminiLiveModel', v)
})

const geminiLanguage = computed({
  get: () => settingsStore.settings.geminiLanguage,
  set: (v: 'auto' | 'zh' | 'en' | 'zh-en') => settingsStore.updateSetting('geminiLanguage', v)
})

const geminiLanguageOptions = computed(() => [
  { label: t('asr.geminiLanguageAuto'), value: 'auto' },
  { label: t('asr.geminiLanguageZh'), value: 'zh' },
  { label: t('asr.geminiLanguageEn'), value: 'en' },
  { label: t('asr.geminiLanguageZhEn'), value: 'zh-en' },
])

// Cohere settings
const cohereApiKey = computed({
  get: () => settingsStore.settings.cohereApiKey,
  set: (v: string) => settingsStore.updateSetting('cohereApiKey', v)
})

const cohereModel = computed({
  get: () => settingsStore.settings.cohereModel,
  set: (v: string) => settingsStore.updateSetting('cohereModel', v)
})

const cohereLanguage = computed({
  get: () => settingsStore.settings.cohereLanguage,
  set: (v: string) => settingsStore.updateSetting('cohereLanguage', v)
})

// Local coli settings
const coliCommandPath = computed({
  get: () => settingsStore.settings.coliCommandPath,
  set: (v: string) => settingsStore.updateSetting('coliCommandPath', v)
})

const coliUseVad = computed({
  get: () => settingsStore.settings.coliUseVad,
  set: (v: boolean) => settingsStore.updateSetting('coliUseVad', v)
})

const coliAsrIntervalMs = computed({
  get: () => settingsStore.settings.coliAsrIntervalMs,
  set: (v: number | null) => settingsStore.updateSetting('coliAsrIntervalMs', v ?? 1000)
})

const coliFinalRefinementMode = computed({
  get: () => settingsStore.settings.coliFinalRefinementMode,
  set: (v: 'off' | 'sensevoice' | 'whisper') => settingsStore.updateSetting('coliFinalRefinementMode', v)
})

const coliRealtime = computed({
  get: () => settingsStore.settings.coliRealtime,
  set: (v: boolean) => settingsStore.updateSetting('coliRealtime', v)
})

const coliStatus = ref<LocalAsrStatus | null>(null)
const coliStatusLoading = ref(false)
const coliStatusError = ref('')
let coliStatusRefreshTimer: number | null = null

const providerOptions = computed(() => {
  const coliDetected = coliStatus.value?.available ?? false
  const coliLabel = coliDetected
    ? t('asr.providerColiReady')
    : coliStatusLoading.value
      ? t('asr.providerColiChecking')
      : t('asr.providerColiUnavailable')

  return [
    { label: t('asr.providerVolcengine'), value: 'volcengine' as ProviderValue },
    { label: t('asr.providerGoogle'), value: 'google' as ProviderValue },
    { label: t('asr.providerQwen'), value: 'qwen' as ProviderValue },
    { label: t('asr.providerGemini'), value: 'gemini' as ProviderValue },
    { label: t('asr.providerGeminiLive'), value: 'gemini-live' as ProviderValue },
    { label: t('asr.providerCohere'), value: 'cohere' as ProviderValue },
    {
      label: coliLabel,
      value: 'coli' as ProviderValue,
      disabled: !coliDetected && settingsStore.settings.asrProviderType !== 'coli'
    }
  ]
})

const showColiUnavailableWarning = computed(() =>
  settingsStore.settings.asrProviderType === 'coli' &&
  !coliStatusLoading.value &&
  !!coliStatus.value &&
  !coliStatus.value.available
)

const coliRefinementOptions = computed(() => [
  { label: t('asr.refinementOff'), value: 'off' },
  { label: t('asr.refinementSenseVoice'), value: 'sensevoice' },
  { label: t('asr.refinementWhisper'), value: 'whisper' },
])

async function refreshColiStatus() {
  coliStatusLoading.value = true
  coliStatusError.value = ''
  try {
    coliStatus.value = await invoke<LocalAsrStatus>('probe_local_asr', {
      commandPath: coliCommandPath.value.trim() || null
    })
  } catch (error) {
    coliStatusError.value = error instanceof Error ? error.message : String(error)
  } finally {
    coliStatusLoading.value = false
  }
}

const qwenModelOptions = computed(() => [
  { label: t('asr.qwenModelStable'), value: 'qwen3-asr-flash-realtime' },
  { label: t('asr.qwenModelSnapshot1'), value: 'qwen3-asr-flash-realtime-2026-02-10' },
  { label: t('asr.qwenModelSnapshot2'), value: 'qwen3-asr-flash-realtime-2025-10-27' },
])

const qwenWsUrlOptions = computed(() => [
  { label: t('asr.qwenEndpointBeijing'), value: 'wss://dashscope.aliyuncs.com/api-ws/v1/realtime' },
  { label: t('asr.qwenEndpointSingapore'), value: 'wss://dashscope-intl.aliyuncs.com/api-ws/v1/realtime' },
])

// Common settings
const maxRecordingOptions = computed(() => [
  { label: t('asr.noLimit'), value: 0 },
  { label: t('asr.oneMinute'), value: 1 },
  { label: t('asr.fiveMinutes'), value: 5 },
  { label: t('asr.tenMinutes'), value: 10 },
  { label: t('asr.thirtyMinutes'), value: 30 }
])

const endWindowSize = computed({
  get: () => settingsStore.settings.endWindowSize,
  set: (v: number | null) => settingsStore.updateSetting('endWindowSize', v)
})

const forceToSpeechTime = computed({
  get: () => settingsStore.settings.forceToSpeechTime,
  set: (v: number | null) => settingsStore.updateSetting('forceToSpeechTime', v)
})

const maxRecordingMinutes = computed({
  get: () => settingsStore.settings.maxRecordingMinutes,
  set: (v: number) => settingsStore.updateSetting('maxRecordingMinutes', v)
})

const enableDdc = computed({
  get: () => settingsStore.settings.enableDdc,
  set: (v: boolean) => settingsStore.updateSetting('enableDdc', v)
})

onMounted(() => {
  refreshColiStatus()
})

watch(coliCommandPath, () => {
  if (coliStatusRefreshTimer !== null) {
    clearTimeout(coliStatusRefreshTimer)
  }
  coliStatusRefreshTimer = window.setTimeout(() => {
    refreshColiStatus()
    coliStatusRefreshTimer = null
  }, 350)
})
</script>

<template>
  <div class="page settings-page asr-page">
    <div class="page-header">
      <h1 class="page-title">{{ t('asr.title') }}</h1>
    </div>

    <!-- Provider Selection -->
    <div class="surface-card asr-card">
      <div class="card-header">
        <div class="card-title">{{ t('asr.provider') }}</div>
        <div class="card-sub">{{ t('asr.providerSub') }}</div>
      </div>
      <div class="field-list">
        <div class="field-row">
          <div class="field-text">
            <div class="field-label">{{ t('asr.asrProvider') }}</div>
          </div>
          <NSelect
            v-model:value="asrProviderType"
            :options="providerOptions"
            size="small"
            class="field-control"
          />
        </div>
        <div v-if="showColiUnavailableWarning" class="warning-box">
          {{ t('asr.warningColiUnavailable') }}
        </div>
      </div>
    </div>

    <!-- Volcengine Credentials -->
    <div v-if="isVolcengine" class="surface-card asr-card">
      <div class="card-header">
        <div class="card-title">{{ t('asr.apiCredentials') }}</div>
        <div class="card-sub">Volcengine 豆包 ASR 服务的访问凭证</div>
      </div>
      <div class="field-list">
        <div class="field-row">
          <div class="field-text">
            <div class="field-label">App Key</div>
          </div>
          <NInput v-model:value="asrAppKey" placeholder="Enter App Key" class="field-control" />
        </div>
        <div class="field-row">
          <div class="field-text">
            <div class="field-label">Access Key</div>
          </div>
          <NInput
            v-model:value="asrAccessKey"
            type="password"
            show-password-on="click"
            placeholder="Enter Access Key"
            class="field-control"
          />
        </div>
        <div class="field-row">
          <div class="field-text">
            <div class="field-label">Resource ID</div>
          </div>
          <NInput v-model:value="asrResourceId" placeholder="Enter Resource ID" class="field-control" />
        </div>
      </div>
    </div>

    <!-- Google Configuration -->
    <div v-if="isGoogle" class="surface-card asr-card">
      <div class="card-header">
        <div class="card-title">{{ t('asr.googleCloudConfiguration') }}</div>
        <div class="card-sub">{{ t('asr.googleCloudConfigurationSub') }}</div>
      </div>
      <div class="field-list">
        <div class="field-row sa-json-row">
          <div class="field-text">
            <div class="field-label">{{ t('asr.serviceAccountKey') }}</div>
            <div class="field-note">{{ t('asr.serviceAccountNote') }}</div>
          </div>
          <NInput
            v-model:value="googleSttApiKey"
            type="textarea"
            placeholder='{"type":"service_account","project_id":"...","private_key":"...",...}'
            :autosize="{ minRows: 3, maxRows: 6 }"
            class="field-control"
          />
        </div>
        <div class="field-row">
          <div class="field-text">
            <div class="field-label">{{ t('asr.projectId') }}</div>
            <div class="field-note">{{ t('asr.projectIdNote') }}</div>
          </div>
          <NInput v-model:value="googleSttProjectId" placeholder="e.g. my-project-123456" class="field-control" />
        </div>
        <div class="field-row">
          <div class="field-text">
            <div class="field-label">{{ t('asr.location') }}</div>
            <div class="field-note">{{ t('asr.locationNote') }}</div>
          </div>
          <NSelect
            v-model:value="googleSttLocation"
            :options="googleLocationOptions"
            filterable
            tag
            size="small"
            class="field-control"
          />
        </div>
        <div class="field-row">
          <div class="field-text">
            <div class="field-label">{{ t('asr.language') }}</div>
            <div class="field-note">{{ t('asr.languageNote') }}</div>
          </div>
          <NInput
            v-model:value="googleSttLanguageCode"
            placeholder="cmn-Hans-CN, en-US"
            class="field-control"
          />
        </div>
        <div class="field-row">
          <div class="field-text">
            <div class="field-label">{{ t('asr.endpointing') }}</div>
            <div class="field-note">{{ t('asr.endpointingNote') }}</div>
          </div>
          <NSelect
            v-model:value="googleSttEndpointing"
            :options="googleEndpointingOptions"
            size="small"
            class="field-control"
          />
        </div>
        <div class="field-row">
          <div class="field-text">
            <div class="field-label">{{ t('asr.phraseBoost') }}</div>
            <div class="field-note">{{ t('asr.phraseBoostNote') }}</div>
          </div>
          <NInputNumber
            v-model:value="googleSttPhraseBoost"
            :min="0"
            :max="20"
            :step="1"
            class="field-control short"
          />
        </div>
      </div>
    </div>

    <!-- Qwen Configuration -->
    <div v-if="isQwen" class="surface-card asr-card">
      <div class="card-header">
        <div class="card-title">{{ t('asr.qwenRealtimeConfiguration') }}</div>
        <div class="card-sub">{{ t('asr.qwenRealtimeConfigurationSub') }}</div>
      </div>
      <div class="field-list">
        <div class="field-row">
          <div class="field-text">
            <div class="field-label">{{ t('asr.apiCredentials') }}</div>
            <div class="field-note">填写对应地域的 DashScope API Key。</div>
          </div>
          <NInput
            v-model:value="qwenAsrApiKey"
            type="password"
            show-password-on="click"
            placeholder="sk-..."
            class="field-control"
          />
        </div>
        <div class="field-row">
          <div class="field-text">
            <div class="field-label">{{ t('asr.endpoint') }}</div>
            <div class="field-note">{{ t('asr.endpointNote') }}</div>
          </div>
          <NSelect
            v-model:value="qwenAsrWsUrl"
            :options="qwenWsUrlOptions"
            filterable
            tag
            size="small"
            class="field-control"
          />
        </div>
        <div class="field-row">
          <div class="field-text">
            <div class="field-label">{{ t('asr.model') }}</div>
            <div class="field-note">{{ t('asr.modelNote') }}</div>
          </div>
          <NSelect
            v-model:value="qwenAsrModel"
            :options="qwenModelOptions"
            filterable
            tag
            size="small"
            class="field-control"
          />
        </div>
        <div class="field-row">
          <div class="field-text">
            <div class="field-label">{{ t('asr.languageHint') }}</div>
            <div class="field-note">{{ t('asr.languageHintNote') }}</div>
          </div>
          <NInput
            v-model:value="qwenAsrLanguage"
            placeholder="留空为自动检测"
            class="field-control"
          />
        </div>
      </div>
    </div>

    <!-- Gemini Configuration -->
    <div v-if="isGemini" class="surface-card asr-card">
      <div class="card-header">
        <div class="card-title">{{ t('asr.geminiConfiguration') }}</div>
        <div class="card-sub">{{ t('asr.geminiConfigurationSub') }}</div>
      </div>
      <div class="field-list">
        <div class="field-row">
          <div class="field-text">
            <div class="field-label">{{ t('asr.apiCredentials') }}</div>
            <div class="field-note">{{ t('asr.geminiApiKeyNote') }}</div>
          </div>
          <NInput
            v-model:value="geminiApiKey"
            type="password"
            show-password-on="click"
            placeholder="AIza..."
            class="field-control"
          />
        </div>
        <div class="field-row">
          <div class="field-text">
            <div class="field-label">{{ t('asr.model') }}</div>
            <div class="field-note">{{ t('asr.geminiModelNote') }}</div>
          </div>
          <NInput
            v-model:value="geminiModel"
            placeholder="gemini-3.1-flash-lite-preview"
            class="field-control"
          />
        </div>
        <div class="field-row">
          <div class="field-text">
            <div class="field-label">{{ t('asr.languageHint') }}</div>
            <div class="field-note">{{ t('asr.geminiLanguageNote') }}</div>
          </div>
          <NSelect
            v-model:value="geminiLanguage"
            :options="geminiLanguageOptions"
            size="small"
            class="field-control"
          />
        </div>
      </div>
    </div>

    <!-- Gemini Live Configuration -->
    <div v-if="isGeminiLive" class="surface-card asr-card">
      <div class="card-header">
        <div class="card-title">{{ t('asr.geminiLiveConfiguration') }}</div>
        <div class="card-sub">{{ t('asr.geminiLiveConfigurationSub') }}</div>
      </div>
      <div class="field-list">
        <div class="field-row">
          <div class="field-text">
            <div class="field-label">{{ t('asr.apiCredentials') }}</div>
            <div class="field-note">{{ t('asr.geminiApiKeyNote') }}</div>
          </div>
          <NInput
            v-model:value="geminiApiKey"
            type="password"
            show-password-on="click"
            placeholder="AIza..."
            class="field-control"
          />
        </div>
        <div class="field-row">
          <div class="field-text">
            <div class="field-label">{{ t('asr.model') }}</div>
            <div class="field-note">{{ t('asr.geminiLiveModelNote') }}</div>
          </div>
          <NInput
            v-model:value="geminiLiveModel"
            placeholder="gemini-3.1-flash-live-preview"
            class="field-control"
          />
        </div>
        <div class="field-row">
          <div class="field-text">
            <div class="field-label">{{ t('asr.languageHint') }}</div>
            <div class="field-note">{{ t('asr.geminiLiveLanguageNote') }}</div>
          </div>
          <NSelect
            v-model:value="geminiLanguage"
            :options="geminiLanguageOptions"
            size="small"
            class="field-control"
          />
        </div>
      </div>
    </div>

    <!-- Cohere Configuration -->
    <div v-if="isCohere" class="surface-card asr-card">
      <div class="card-header">
        <div class="card-title">{{ t('asr.cohereConfiguration') }}</div>
        <div class="card-sub">{{ t('asr.cohereConfigurationSub') }}</div>
      </div>
      <div class="field-list">
        <div class="field-row">
          <div class="field-text">
            <div class="field-label">{{ t('asr.apiCredentials') }}</div>
            <div class="field-note">{{ t('asr.cohereApiKeyNote') }}</div>
          </div>
          <NInput
            v-model:value="cohereApiKey"
            type="password"
            show-password-on="click"
            placeholder="Enter Cohere API key"
            class="field-control"
          />
        </div>
        <div class="field-row">
          <div class="field-text">
            <div class="field-label">{{ t('asr.model') }}</div>
            <div class="field-note">{{ t('asr.cohereModelNote') }}</div>
          </div>
          <NInput
            v-model:value="cohereModel"
            placeholder="cohere-transcribe-03-2026"
            class="field-control"
          />
        </div>
        <div class="field-row">
          <div class="field-text">
            <div class="field-label">{{ t('asr.languageHint') }}</div>
            <div class="field-note">{{ t('asr.cohereLanguageNote') }}</div>
          </div>
          <NInput
            v-model:value="cohereLanguage"
            placeholder="zh"
            class="field-control"
          />
        </div>
      </div>
    </div>

    <div v-if="isColi" class="surface-card asr-card">
      <div class="card-header">
        <div class="card-title">{{ t('asr.localColiConfiguration') }}</div>
        <div class="card-sub">{{ t('asr.localColiConfigurationSub') }}</div>
      </div>
      <div class="field-list">
        <div class="field-row">
          <div class="field-text">
            <div class="field-label">{{ t('asr.detectionStatus') }}</div>
            <div class="field-note">
              {{ coliStatusError || coliStatus?.message || t('asr.checkingLocalColi') }}
            </div>
          </div>
          <div
            class="status-pill"
            :class="{
              online: coliStatus?.available,
              offline: !coliStatusLoading && !coliStatus?.available
            }"
          >
            {{ coliStatusLoading ? t('asr.checking') : coliStatus?.available ? t('asr.detected') : t('asr.notFound') }}
          </div>
        </div>
        <div class="field-row">
          <div class="field-text">
            <div class="field-label">{{ t('asr.commandPath') }}</div>
            <div class="field-note">{{ t('asr.commandPathNote') }}</div>
          </div>
          <div class="field-control action-control">
            <NInput
              v-model:value="coliCommandPath"
              :placeholder="t('asr.leaveEmptyToAuto')"
            />
            <NButton secondary size="small" @click="refreshColiStatus">
              {{ t('asr.refresh') }}
            </NButton>
          </div>
        </div>
        <div class="field-row">
          <div class="field-text">
            <div class="field-label">{{ t('asr.resolvedPath') }}</div>
            <div class="field-note">{{ t('asr.resolvedPathNote') }}</div>
          </div>
          <div class="field-value mono">
            {{ coliStatus?.resolvedPath || '—' }}
          </div>
        </div>
        <div class="field-row">
          <div class="field-text">
            <div class="field-label">{{ t('asr.modelCache') }}</div>
            <div class="field-note">{{ t('asr.modelCacheNote') }}</div>
          </div>
          <div class="field-value mono">
            {{ coliStatus?.modelsDir || '—' }}
          </div>
        </div>
        <div class="field-row">
          <div class="field-text">
            <div class="field-label">{{ t('asr.realtimeStreaming') }}</div>
            <div class="field-note">{{ t('asr.realtimeStreamingNote') }}</div>
          </div>
          <NCheckbox v-model:checked="coliRealtime" />
        </div>
        <div v-if="coliRealtime" class="field-row">
          <div class="field-text">
            <div class="field-label">{{ t('asr.enableVad') }}</div>
            <div class="field-note">{{ t('asr.enableVadNote') }}</div>
          </div>
          <NCheckbox v-model:checked="coliUseVad" />
        </div>
        <div v-if="coliRealtime && !coliUseVad" class="warning-box">
          {{ t('asr.warningVadOff') }}
        </div>
        <div v-if="coliRealtime && !coliUseVad" class="field-row">
          <div class="field-text">
            <div class="field-label">{{ t('asr.streamingInterval') }}</div>
            <div class="field-note">{{ t('asr.streamingIntervalNote') }}</div>
          </div>
          <NInputNumber
            v-model:value="coliAsrIntervalMs"
            :min="200"
            :max="5000"
            :step="100"
            class="field-control short"
          />
        </div>
        <div v-if="coliRealtime" class="field-row">
          <div class="field-text">
            <div class="field-label">{{ t('asr.finalRefinement') }}</div>
            <div class="field-note">{{ t('asr.finalRefinementNote') }}</div>
          </div>
          <NSelect
            v-model:value="coliFinalRefinementMode"
            :options="coliRefinementOptions"
            size="small"
            class="field-control"
          />
        </div>
        <div class="field-row">
          <div class="field-text">
            <div class="field-label">{{ t('asr.installedModels') }}</div>
            <div class="field-note">{{ t('asr.installedModelsNote') }}</div>
          </div>
          <div class="pill-group">
            <span class="mini-pill" :class="{ ready: coliStatus?.sensevoiceInstalled }">
              {{ coliStatus?.sensevoiceInstalled ? t('asr.senseVoiceReady') : t('asr.senseVoicePending') }}
            </span>
            <span class="mini-pill" :class="{ ready: coliStatus?.whisperInstalled }">
              {{ coliStatus?.whisperInstalled ? t('asr.whisperReady') : t('asr.whisperPending') }}
            </span>
            <span class="mini-pill" :class="{ ready: coliStatus?.vadInstalled }">
              {{ coliStatus?.vadInstalled ? t('asr.sileroReady') : t('asr.sileroPending') }}
            </span>
            <span class="mini-pill" :class="{ ready: coliStatus?.ffmpegAvailable }">
              {{ coliStatus?.ffmpegAvailable ? t('asr.ffmpegReady') : t('asr.ffmpegMissing') }}
            </span>
          </div>
        </div>
      </div>
    </div>

    <!-- Volcengine Recognition Settings -->
    <div class="surface-card asr-card">
      <div class="card-header">
        <div class="card-title">{{ t('asr.recognition') }}</div>
        <div class="card-sub">{{ t('asr.recognitionSub') }}</div>
      </div>
      <div class="field-list">
        <div v-if="isVolcengine" class="field-row">
          <div class="field-text">
            <div class="field-label">{{ t('asr.recognitionMode') }}</div>
          </div>
          <NSelect
            v-model:value="recognitionMode"
            :options="recognitionModeOptions"
            size="small"
            class="field-control"
          />
        </div>
        <div class="field-row">
          <div class="field-text">
            <div class="field-label">{{ t('asr.maxRecordingDuration') }}</div>
            <div class="field-note">{{ t('asr.maxRecordingDurationNote') }}</div>
          </div>
          <NSelect
            v-model:value="maxRecordingMinutes"
            :options="maxRecordingOptions"
            size="small"
            class="field-control short"
          />
        </div>
        <div v-if="isVolcengine" class="field-row">
          <div class="field-text">
            <div class="field-label">{{ t('asr.enableSemanticSmoothing') }}</div>
            <div class="field-note">{{ t('asr.enableSemanticSmoothingNote') }}</div>
          </div>
          <NCheckbox v-model:checked="enableDdc" />
        </div>
        <div v-if="isVolcengine" class="field-row">
          <div class="field-text">
            <div class="field-label">{{ t('asr.webSocketUrl') }}</div>
            <div class="field-note">{{ t('asr.webSocketUrlNote') }}</div>
          </div>
          <NInput v-model:value="asrWsUrl" placeholder="wss://openspeech.bytedance.com/api/v3/..." class="field-control" />
        </div>
      </div>
    </div>

    <!-- Volcengine Endpoint Settings -->
    <div v-if="isVolcengine" class="surface-card asr-card">
      <div class="card-header">
        <div class="card-title">{{ t('asr.endpointSettings') }}</div>
        <div class="card-sub">{{ t('asr.endpointSettingsSub') }}</div>
      </div>
      <div class="field-list">
        <div class="field-row">
          <div class="field-text">
            <div class="field-label">{{ t('asr.endWindowSize') }}</div>
            <div class="field-note">{{ t('asr.useServiceDefault') }}</div>
          </div>
          <NInputNumber
            v-model:value="endWindowSize"
            :min="0"
            :max="5000"
            :placeholder="t('asr.serviceDefault')"
            class="field-control short"
            clearable
          />
        </div>
        <div class="field-row">
          <div class="field-text">
            <div class="field-label">{{ t('asr.forceToSpeechTime') }}</div>
            <div class="field-note">{{ t('asr.useServiceDefault') }}</div>
          </div>
          <NInputNumber
            v-model:value="forceToSpeechTime"
            :min="0"
            :max="60000"
            :placeholder="t('asr.serviceDefault')"
            class="field-control short"
            clearable
          />
        </div>
      </div>
    </div>

  </div>
</template>

<style scoped>
.settings-page {
  width: 100%;
  max-width: 1120px;
  padding-bottom: var(--spacing-2xl);
}

.asr-card {
  padding: var(--spacing-lg) var(--spacing-xl);
  background: var(--color-bg-secondary);
  border: 1px solid var(--color-border);
}

.card-header {
  display: flex;
  flex-direction: column;
  gap: 4px;
  margin-bottom: var(--spacing-md);
}

.card-title {
  font-size: var(--font-lg);
  font-weight: 700;
}

.card-sub {
  color: var(--color-text-tertiary);
  font-size: var(--font-xs);
}

.field-list {
  display: flex;
  flex-direction: column;
  gap: var(--spacing-md);
}

.field-row {
  display: flex;
  align-items: center;
  justify-content: space-between;
  gap: var(--spacing-lg);
}

.field-row + .field-row {
  padding-top: 4px;
}

.field-text {
  display: flex;
  flex-direction: column;
  gap: 4px;
  flex: 1;
}

.field-label {
  font-weight: 600;
  color: var(--color-text-primary);
}

.field-note {
  font-size: var(--font-xs);
  color: var(--color-text-tertiary);
  max-width: 520px;
}

.field-control {
  width: 420px;
  max-width: 100%;
}

.field-control.short {
  width: 200px;
}

.action-control {
  display: flex;
  align-items: center;
  gap: var(--spacing-sm);
}

.sa-json-row {
  align-items: flex-start;
}

.field-value {
  width: 420px;
  max-width: 100%;
  color: var(--color-text-secondary);
  font-size: var(--font-sm);
  text-align: right;
}

.mono {
  font-family: 'SF Mono', 'Menlo', monospace;
  font-size: var(--font-xs);
  word-break: break-all;
}

.status-pill {
  min-width: 96px;
  padding: 6px 10px;
  border-radius: 999px;
  border: 1px solid var(--color-border);
  font-size: var(--font-xs);
  font-weight: 700;
  text-align: center;
  color: var(--color-text-secondary);
  background: color-mix(in srgb, var(--color-bg-secondary) 85%, transparent);
}

.status-pill.online {
  color: #0d6b42;
  border-color: color-mix(in srgb, #0d6b42 35%, var(--color-border));
  background: color-mix(in srgb, #0d6b42 10%, var(--color-bg-secondary));
}

.status-pill.offline {
  color: #9a3412;
  border-color: color-mix(in srgb, #9a3412 35%, var(--color-border));
  background: color-mix(in srgb, #9a3412 10%, var(--color-bg-secondary));
}

.pill-group {
  display: flex;
  flex-wrap: wrap;
  justify-content: flex-end;
  gap: var(--spacing-sm);
  width: 420px;
  max-width: 100%;
}

.mini-pill {
  padding: 6px 10px;
  border-radius: 999px;
  border: 1px solid var(--color-border);
  font-size: var(--font-xs);
  color: var(--color-text-secondary);
}

.mini-pill.ready {
  color: #0d6b42;
  border-color: color-mix(in srgb, #0d6b42 35%, var(--color-border));
  background: color-mix(in srgb, #0d6b42 10%, var(--color-bg-secondary));
}

.warning-box {
  padding: 10px 12px;
  border-radius: 12px;
  border: 1px solid color-mix(in srgb, #9a3412 35%, var(--color-border));
  background: color-mix(in srgb, #9a3412 10%, var(--color-bg-secondary));
  color: #9a3412;
  font-size: var(--font-xs);
  line-height: 1.5;
}
</style>
