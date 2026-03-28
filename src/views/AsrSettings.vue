<script setup lang="ts">
import { computed, onMounted, ref, watch } from 'vue'
import { invoke } from '@tauri-apps/api/core'
import { NSelect } from 'naive-ui'
import { useI18n } from 'vue-i18n'
import { useSettingsStore } from '../stores/settings'
import type { LocalAsrStatus } from '../types/asr'
import AsrVolcengineSettings from '../components/asr/AsrVolcengineSettings.vue'
import AsrGoogleSettings from '../components/asr/AsrGoogleSettings.vue'
import AsrQwenSettings from '../components/asr/AsrQwenSettings.vue'
import AsrGeminiSettings from '../components/asr/AsrGeminiSettings.vue'
import AsrGeminiLiveSettings from '../components/asr/AsrGeminiLiveSettings.vue'
import AsrCohereSettings from '../components/asr/AsrCohereSettings.vue'
import AsrSonioxSettings from '../components/asr/AsrSonioxSettings.vue'
import AsrColiSettings from '../components/asr/AsrColiSettings.vue'

const settingsStore = useSettingsStore()
const { t } = useI18n()

type ProviderValue = 'volcengine' | 'google' | 'qwen' | 'gemini' | 'gemini-live' | 'cohere' | 'soniox' | 'coli'

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
    <AsrSonioxSettings v-if="isSoniox" />
    <AsrColiSettings
      v-if="isColi"
      :coli-status="coliStatus"
      :coli-status-loading="coliStatusLoading"
      :coli-status-error="coliStatusError"
      @refresh="refreshColiStatus"
    />

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
</style>
