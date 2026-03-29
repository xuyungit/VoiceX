<script setup lang="ts">
import { computed } from 'vue'
import { NInput, NSelect } from 'naive-ui'
import { useI18n } from 'vue-i18n'
import { useSettingsStore } from '../../stores/settings'

const settingsStore = useSettingsStore()
const { t } = useI18n()

const openaiAsrApiKey = computed({
  get: () => settingsStore.settings.openaiAsrApiKey,
  set: (v: string) => settingsStore.updateSetting('openaiAsrApiKey', v)
})

const openaiAsrMode = computed({
  get: () => settingsStore.settings.openaiAsrMode,
  set: (v: 'batch' | 'realtime') => settingsStore.updateSetting('openaiAsrMode', v)
})

const openaiAsrModel = computed({
  get: () => settingsStore.settings.openaiAsrModel,
  set: (v: string) => settingsStore.updateSetting('openaiAsrModel', v)
})

const openaiAsrBaseUrl = computed({
  get: () => settingsStore.settings.openaiAsrBaseUrl,
  set: (v: string) => settingsStore.updateSetting('openaiAsrBaseUrl', v)
})

const openaiAsrLanguage = computed({
  get: () => settingsStore.settings.openaiAsrLanguage,
  set: (v: string) => settingsStore.updateSetting('openaiAsrLanguage', v)
})

const openaiAsrPrompt = computed({
  get: () => settingsStore.settings.openaiAsrPrompt,
  set: (v: string) => settingsStore.updateSetting('openaiAsrPrompt', v)
})

const openaiModelOptions = computed(() => [
  { label: 'GPT-4o Transcribe', value: 'gpt-4o-transcribe' },
  { label: 'GPT-4o Mini Transcribe', value: 'gpt-4o-mini-transcribe' },
  { label: 'Whisper-1', value: 'whisper-1' },
])

const openaiModeOptions = computed(() => [
  { label: t('asr.openaiModeBatch'), value: 'batch' },
  { label: t('asr.openaiModeRealtime'), value: 'realtime' },
])
</script>

<template>
  <div class="surface-card asr-card">
    <div class="card-header">
      <div class="card-title">{{ t('asr.openaiConfiguration') }}</div>
      <div class="card-sub">{{ t('asr.openaiConfigurationSub') }}</div>
    </div>
    <div class="field-list">
      <div class="field-row">
        <div class="field-text">
          <div class="field-label">{{ t('asr.apiCredentials') }}</div>
          <div class="field-note">{{ t('asr.openaiApiKeyNote') }}</div>
        </div>
        <NInput
          v-model:value="openaiAsrApiKey"
          type="password"
          show-password-on="click"
          placeholder="sk-..."
          class="field-control"
        />
      </div>
      <div class="field-row">
        <div class="field-text">
          <div class="field-label">{{ t('asr.recognitionMode') }}</div>
          <div class="field-note">{{ t('asr.openaiModeNote') }}</div>
        </div>
        <NSelect
          v-model:value="openaiAsrMode"
          :options="openaiModeOptions"
          size="small"
          class="field-control"
        />
      </div>
      <div class="field-row">
        <div class="field-text">
          <div class="field-label">{{ t('asr.model') }}</div>
          <div class="field-note">{{ t('asr.openaiModelNote') }}</div>
        </div>
        <NSelect
          v-model:value="openaiAsrModel"
          :options="openaiModelOptions"
          size="small"
          class="field-control"
        />
      </div>
      <div class="field-row">
        <div class="field-text">
          <div class="field-label">{{ t('asr.endpoint') }}</div>
          <div class="field-note">{{ t('asr.openaiBaseUrlNote') }}</div>
        </div>
        <NInput
          v-model:value="openaiAsrBaseUrl"
          placeholder="https://api.openai.com/v1"
          class="field-control"
        />
      </div>
      <div class="field-row">
        <div class="field-text">
          <div class="field-label">{{ t('asr.languageHint') }}</div>
          <div class="field-note">{{ t('asr.openaiLanguageNote') }}</div>
        </div>
        <NInput
          v-model:value="openaiAsrLanguage"
          placeholder="zh"
          class="field-control"
        />
      </div>
      <div class="field-row align-start">
        <div class="field-text">
          <div class="field-label">{{ t('asr.prompt') }}</div>
        <div class="field-note">{{ t('asr.openaiPromptNote') }}</div>
        </div>
        <NInput
          v-model:value="openaiAsrPrompt"
          type="textarea"
          :autosize="{ minRows: 3, maxRows: 6 }"
          class="field-control"
        />
      </div>
    </div>
  </div>
</template>

<style scoped>
@import '../../styles/asr-settings.css';
</style>
