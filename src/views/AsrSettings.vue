<script setup lang="ts">
import { computed, onMounted, ref, watch } from 'vue'
import { invoke } from '@tauri-apps/api/core'
import { NButton, NSelect } from 'naive-ui'
import { useI18n } from 'vue-i18n'
import { useSettingsStore } from '../stores/settings'
import type { LocalAsrStatus } from '../types/asr'
import AsrVolcengineSettings from '../components/asr/AsrVolcengineSettings.vue'
import AsrGoogleSettings from '../components/asr/AsrGoogleSettings.vue'
import AsrQwenSettings from '../components/asr/AsrQwenSettings.vue'
import AsrGeminiSettings from '../components/asr/AsrGeminiSettings.vue'
import AsrGeminiLiveSettings from '../components/asr/AsrGeminiLiveSettings.vue'
import AsrCohereSettings from '../components/asr/AsrCohereSettings.vue'
import AsrOpenAISettings from '../components/asr/AsrOpenAISettings.vue'
import AsrSonioxSettings from '../components/asr/AsrSonioxSettings.vue'
import AsrColiSettings from '../components/asr/AsrColiSettings.vue'

const settingsStore = useSettingsStore()
const { t } = useI18n()

type ProviderValue = 'volcengine' | 'google' | 'qwen' | 'gemini' | 'gemini-live' | 'cohere' | 'openai' | 'soniox' | 'coli'

interface AsrProviderProbeResult {
  provider: string
  ok: boolean
  recognitionTimeMs: number | null
  recognitionResult: string
  errorMessage: string | null
}

// --- Coli status probe ---
const coliStatus = ref<LocalAsrStatus | null>(null)
const coliStatusLoading = ref(false)
const coliStatusError = ref('')
let coliStatusRefreshTimer: number | null = null

async function refreshColiStatus() {
  coliStatusLoading.value = true
  coliStatusError.value = ''
  try {
    coliStatus.value = await invoke<LocalAsrStatus>('probe_local_asr', {
      commandPath: settingsStore.settings.coliCommandPath.trim() || null
    })
  } catch (error) {
    coliStatusError.value = error instanceof Error ? error.message : String(error)
  } finally {
    coliStatusLoading.value = false
  }
}

watch(() => settingsStore.settings.coliCommandPath, () => {
  if (coliStatusRefreshTimer !== null) {
    clearTimeout(coliStatusRefreshTimer)
  }
  coliStatusRefreshTimer = window.setTimeout(() => {
    refreshColiStatus()
    coliStatusRefreshTimer = null
  }, 350)
})

onMounted(() => {
  refreshColiStatus()
})

// --- Provider selection ---
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
const isOpenAI = computed(() => settingsStore.settings.asrProviderType === 'openai')
const isSoniox = computed(() => settingsStore.settings.asrProviderType === 'soniox')
const isColi = computed(() => settingsStore.settings.asrProviderType === 'coli')

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
    { label: t('asr.providerOpenAI'), value: 'openai' as ProviderValue },
    { label: t('asr.providerSoniox'), value: 'soniox' as ProviderValue },
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

const providerProbeLoading = ref(false)
const providerProbeResult = ref<AsrProviderProbeResult | null>(null)
const providerProbeError = ref('')
const probeAudioLoading = ref(false)
const isProbeAudioPlaying = ref(false)
const probeAudioPlayer = ref<HTMLAudioElement | null>(null)
const probeAudioUrl = ref<string | null>(null)

async function runProviderProbe() {
  providerProbeLoading.value = true
  providerProbeError.value = ''
  try {
    await settingsStore.forceSaveSettings()
    providerProbeResult.value = await invoke<AsrProviderProbeResult>('probe_current_asr_provider')
  } catch (error) {
    providerProbeResult.value = null
    providerProbeError.value = error instanceof Error ? error.message : String(error)
  } finally {
    providerProbeLoading.value = false
  }
}

async function toggleProbeAudioPlayback() {
  if (isProbeAudioPlaying.value) {
    stopProbeAudioPlayback()
    return
  }

  probeAudioLoading.value = true
  try {
    stopProbeAudioPlayback()
    const bytes = await invoke<number[]>('load_provider_probe_audio')
    const buffer = new Uint8Array(bytes)
    const url = URL.createObjectURL(new Blob([buffer], { type: 'audio/ogg' }))
    probeAudioUrl.value = url

    const audio = new Audio(url)
    probeAudioPlayer.value = audio
    audio.onended = () => {
      stopProbeAudioPlayback()
    }
    audio.onerror = () => {
      stopProbeAudioPlayback()
    }

    await audio.play()
    isProbeAudioPlaying.value = true
  } catch (error) {
    stopProbeAudioPlayback()
    providerProbeError.value = error instanceof Error ? error.message : String(error)
  } finally {
    probeAudioLoading.value = false
  }
}

function stopProbeAudioPlayback() {
  if (probeAudioPlayer.value) {
    probeAudioPlayer.value.pause()
    probeAudioPlayer.value.currentTime = 0
    probeAudioPlayer.value = null
  }
  isProbeAudioPlaying.value = false
  if (probeAudioUrl.value) {
    URL.revokeObjectURL(probeAudioUrl.value)
    probeAudioUrl.value = null
  }
}

// --- Common settings ---
const maxRecordingOptions = computed(() => [
  { label: t('asr.noLimit'), value: 0 },
  { label: t('asr.oneMinute'), value: 1 },
  { label: t('asr.fiveMinutes'), value: 5 },
  { label: t('asr.tenMinutes'), value: 10 },
  { label: t('asr.thirtyMinutes'), value: 30 }
])

const maxRecordingMinutes = computed({
  get: () => settingsStore.settings.maxRecordingMinutes,
  set: (v: number) => settingsStore.updateSetting('maxRecordingMinutes', v)
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

    <!-- Provider-specific configuration -->
    <AsrVolcengineSettings v-if="isVolcengine" />
    <AsrGoogleSettings v-if="isGoogle" />
    <AsrQwenSettings v-if="isQwen" />
    <AsrGeminiSettings v-if="isGemini" />
    <AsrGeminiLiveSettings v-if="isGeminiLive" />
    <AsrCohereSettings v-if="isCohere" />
    <AsrOpenAISettings v-if="isOpenAI" />
    <AsrSonioxSettings v-if="isSoniox" />
    <AsrColiSettings
      v-if="isColi"
      :coli-status="coliStatus"
      :coli-status-loading="coliStatusLoading"
      :coli-status-error="coliStatusError"
      @refresh="refreshColiStatus"
    />

    <div class="surface-card asr-card">
      <div class="card-header">
        <div class="card-title">{{ t('asr.providerProbe') }}</div>
        <div class="card-sub">{{ t('asr.providerProbeSub') }}</div>
      </div>
      <div class="field-list">
        <div class="field-row align-start">
          <div class="field-text">
            <div class="field-label">{{ t('asr.providerProbeButton') }}</div>
            <div class="field-note">{{ t('asr.providerProbeNote') }}</div>
          </div>
          <div class="probe-actions">
            <NButton
              :loading="probeAudioLoading"
              size="small"
              @click="toggleProbeAudioPlayback"
            >
              {{ isProbeAudioPlaying ? t('asr.providerProbeStopAudio') : t('asr.providerProbePlayAudio') }}
            </NButton>
            <NButton
              :loading="providerProbeLoading"
              type="primary"
              secondary
              size="small"
              @click="runProviderProbe"
            >
              {{ t('asr.providerProbeButton') }}
            </NButton>
          </div>
        </div>

        <div v-if="providerProbeResult" class="probe-result" :class="{ ok: providerProbeResult.ok, error: !providerProbeResult.ok }">
          <div class="probe-line">
            <span>{{ t('asr.providerProbeStatus') }}</span>
            <strong>{{ providerProbeResult.ok ? t('asr.providerProbeStatusOk') : t('asr.providerProbeStatusFailed') }}</strong>
          </div>
          <div v-if="providerProbeResult.recognitionTimeMs !== null" class="probe-line">
            <span>{{ t('asr.providerProbeLatency') }}</span>
            <strong>{{ providerProbeResult.recognitionTimeMs }} ms</strong>
          </div>
          <div class="probe-result-label">{{ t('asr.providerProbeTranscript') }}</div>
          <div class="probe-message">
            {{ providerProbeResult.recognitionResult || t('asr.providerProbeTranscriptEmpty') }}
          </div>
        </div>

        <div v-if="providerProbeResult?.errorMessage" class="warning-box">
          {{ providerProbeResult.errorMessage }}
        </div>

        <div v-if="providerProbeError" class="warning-box">
          {{ providerProbeError }}
        </div>
      </div>
    </div>

    <!-- Common Recording Settings -->
    <div class="surface-card asr-card">
      <div class="card-header">
        <div class="card-title">{{ t('asr.recognition') }}</div>
        <div class="card-sub">{{ t('asr.recognitionSub') }}</div>
      </div>
      <div class="field-list">
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
      </div>
    </div>
  </div>
</template>

<style scoped>
@import '../styles/asr-settings.css';

.settings-page {
  width: 100%;
  max-width: 1120px;
  padding-bottom: var(--spacing-2xl);
}

.probe-result {
  padding: 12px 14px;
  border-radius: 12px;
  border: 1px solid rgba(255, 255, 255, 0.08);
  background: rgba(255, 255, 255, 0.03);
  display: grid;
  gap: 8px;
}

.probe-actions {
  display: flex;
  align-items: center;
  gap: 8px;
}

.probe-result.ok {
  border-color: rgba(74, 222, 128, 0.28);
  background: rgba(74, 222, 128, 0.08);
}

.probe-result.error {
  border-color: rgba(248, 113, 113, 0.28);
  background: rgba(248, 113, 113, 0.08);
}

.probe-line {
  display: flex;
  align-items: center;
  justify-content: space-between;
  gap: 12px;
  font-size: 12px;
  color: var(--text-secondary);
}

.probe-line strong {
  color: var(--text-primary);
  text-align: right;
  word-break: break-all;
}

.probe-result-label {
  font-size: 12px;
  color: var(--text-secondary);
}

.probe-message {
  font-size: 12px;
  line-height: 1.5;
  color: var(--text-primary);
  word-break: break-word;
}
</style>
