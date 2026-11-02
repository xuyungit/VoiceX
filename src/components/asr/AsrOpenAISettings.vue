<script setup lang="ts">
import { computed } from 'vue'
import AsrModelSelect from './AsrModelSelect.vue'
import { findAsrModel, openAiModelForMode } from '../../utils/asrModels'
import { NInput, NSelect } from 'naive-ui'
import { useI18n } from 'vue-i18n'
import { useSettingsStore } from '../../stores/settings'
import {
  buildOpenAiPostRecordingRefineOptions,
  buildOpenAiRecognitionModeOptions,
  normalizeBatchCapablePostRecordingRefine,
} from '../../utils/providerOptions'

const settingsStore = useSettingsStore()
const { t } = useI18n()

const openaiAsrApiKey = computed({
  get: () => settingsStore.settings.openaiAsrApiKey,
  set: (v: string) => settingsStore.updateSetting('openaiAsrApiKey', v)
})

const openaiAsrMode = computed({
  get: () => settingsStore.settings.openaiAsrMode,
  set: (v: 'batch' | 'realtime') => {
    if (v === settingsStore.settings.openaiAsrMode) return
    settingsStore.updateSetting('openaiAsrModel', openAiModelForMode(settingsStore.settings.openaiAsrModel, v))
    settingsStore.updateSetting('openaiAsrMode', v)
    settingsStore.updateSetting(
      'openaiAsrPostRecordingRefine',
      normalizeBatchCapablePostRecordingRefine(v, settingsStore.settings.openaiAsrPostRecordingRefine)
    )
  }
})

const openaiAsrPostRecordingRefine = computed({
  get: () => settingsStore.settings.openaiAsrPostRecordingRefine,
  set: (v: 'off' | 'batch_refine') => settingsStore.updateSetting(
    'openaiAsrPostRecordingRefine',
    normalizeBatchCapablePostRecordingRefine(openaiAsrMode.value, v)
  )
})

const openaiAsrModel = computed({
  get: () => settingsStore.settings.openaiAsrModel,
  set: (v: string) => settingsStore.updateSetting('openaiAsrModel', v)
})

const openaiAsrRefineModel = computed({
  get: () => settingsStore.settings.openaiAsrRefineModel,
  set: (v: string) => settingsStore.updateSetting('openaiAsrRefineModel', v)
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

const openaiAsrDelay = computed({
  get: () => settingsStore.settings.openaiAsrDelay,
  set: (v: '' | 'minimal' | 'low' | 'medium' | 'high' | 'xhigh') =>
    settingsStore.updateSetting('openaiAsrDelay', v)
})


const openaiDelayOptions = computed(() => [
  { label: t('asr.openaiDelayDefault'), value: '' },
  { label: 'minimal', value: 'minimal' },
  { label: 'low', value: 'low' },
  { label: 'medium', value: 'medium' },
  { label: 'high', value: 'high' },
  { label: 'xhigh', value: 'xhigh' },
])

// `delay` only exists on the realtime transcription session, and only the
// newer models accept it.
const showDelay = computed(
  () => openaiAsrMode.value === 'realtime' && !!findAsrModel('openai', openaiAsrModel.value)?.delay
)
const committedRealtime = computed(() => openaiAsrMode.value === 'realtime' &&
  findAsrModel('openai', openaiAsrModel.value)?.id === 'gpt-transcribe')

const openaiModeOptions = computed(() => buildOpenAiRecognitionModeOptions(t))
const openaiPostRecordingRefineOptions = computed(() => buildOpenAiPostRecordingRefineOptions(t))
</script>

<template>
  <div class="surface-card asr-card">
    <div class="card-header">
      <div class="card-title">{{ t('asr.openaiConfiguration') }}</div>
    </div>
    <div class="field-list">
      <div class="field-row">
        <div class="field-text">
          <div class="field-label">{{ t('asr.apiCredentials') }}</div>
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
          <div class="field-label">{{ t(openaiAsrMode === 'realtime' ? 'asr.openaiRealtimeModel' : 'asr.openaiBatchModel') }}</div>
          <div v-if="committedRealtime" class="field-note">{{ t('asr.openaiCommittedRealtimeNote') }}</div>
        </div>
        <AsrModelSelect v-model:value="openaiAsrModel" provider="openai" :mode="openaiAsrMode" class="field-control" />
      </div>
      <div v-if="showDelay" class="field-row">
        <div class="field-text">
          <div class="field-label">{{ t('asr.openaiDelay') }}</div>
          <div class="field-note">{{ t('asr.openaiDelayShortNote') }}</div>
        </div>
        <NSelect v-model:value="openaiAsrDelay" :options="openaiDelayOptions" size="small" class="field-control" />
      </div>
      <div v-if="openaiAsrMode === 'realtime'" class="field-row">
        <div class="field-text">
          <div class="field-label">{{ t('asr.postRecordingRefine') }}</div>
          <div class="field-note">{{ t('asr.openaiRefineShortNote') }}</div>
        </div>
        <NSelect v-model:value="openaiAsrPostRecordingRefine" :options="openaiPostRecordingRefineOptions" size="small" class="field-control" />
      </div>
      <div v-if="openaiAsrMode === 'realtime' && openaiAsrPostRecordingRefine === 'batch_refine'" class="field-row">
        <div class="field-text">
          <div class="field-label">{{ t('asr.refineModel') }}</div>
        </div>
        <AsrModelSelect v-model:value="openaiAsrRefineModel" provider="openai" mode="batch" class="field-control" />
      </div>
      <div class="field-row">
        <div class="field-text">
          <div class="field-label">{{ t('asr.endpoint') }}</div>
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
          <div class="field-note">{{ t('asr.openaiLanguageShortNote') }}</div>
        </div>
        <NInput
          v-model:value="openaiAsrLanguage"
          placeholder="zh, en"
          class="field-control"
        />
      </div>
      <div class="field-row align-start">
        <div class="field-text">
          <div class="field-label">{{ t('asr.prompt') }}</div>
          <div class="field-note">{{ t('asr.openaiPromptShortNote') }}</div>
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
.asr-card { container-type: inline-size; }
.field-control { flex: 0 1 420px; min-width: 0; }
.field-text { min-width: 0; }
@container (max-width: 560px) {
  .field-row { flex-direction: column; align-items: stretch; gap: 6px; }
  .field-control { flex: auto; width: 100%; }
}
</style>
