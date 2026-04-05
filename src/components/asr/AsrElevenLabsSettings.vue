<script setup lang="ts">
import { computed } from 'vue'
import { NCheckbox, NInput, NSelect } from 'naive-ui'
import { useI18n } from 'vue-i18n'
import { useSettingsStore } from '../../stores/settings'
import {
  buildElevenLabsPostRecordingRefineOptions,
  buildElevenLabsRecognitionModeOptions,
  ELEVENLABS_BATCH_MODEL_OPTIONS,
  ELEVENLABS_REALTIME_MODEL_OPTIONS,
  normalizeBatchCapablePostRecordingRefine
} from '../../utils/providerOptions'

const settingsStore = useSettingsStore()
const { t } = useI18n()

const elevenlabsApiKey = computed({
  get: () => settingsStore.settings.elevenlabsApiKey,
  set: (value: string) => settingsStore.updateSetting('elevenlabsApiKey', value)
})

const elevenlabsRecognitionMode = computed({
  get: () => settingsStore.settings.elevenlabsRecognitionMode,
  set: (value: 'realtime' | 'batch') => {
    settingsStore.updateSetting('elevenlabsRecognitionMode', value)
    settingsStore.updateSetting(
      'elevenlabsPostRecordingRefine',
      normalizeBatchCapablePostRecordingRefine(
        value,
        settingsStore.settings.elevenlabsPostRecordingRefine
      )
    )
  }
})

const elevenlabsPostRecordingRefine = computed({
  get: () => settingsStore.settings.elevenlabsPostRecordingRefine,
  set: (value: 'off' | 'batch_refine') => {
    settingsStore.updateSetting(
      'elevenlabsPostRecordingRefine',
      normalizeBatchCapablePostRecordingRefine(elevenlabsRecognitionMode.value, value)
    )
  }
})

const elevenlabsRealtimeModel = computed({
  get: () => settingsStore.settings.elevenlabsRealtimeModel,
  set: (value: string) => settingsStore.updateSetting('elevenlabsRealtimeModel', value)
})

const elevenlabsBatchModel = computed({
  get: () => settingsStore.settings.elevenlabsBatchModel,
  set: (value: string) => settingsStore.updateSetting('elevenlabsBatchModel', value)
})

const elevenlabsLanguage = computed({
  get: () => settingsStore.settings.elevenlabsLanguage,
  set: (value: string) => settingsStore.updateSetting('elevenlabsLanguage', value)
})

const elevenlabsEnableKeyterms = computed({
  get: () => settingsStore.settings.elevenlabsEnableKeyterms,
  set: (value: boolean) => settingsStore.updateSetting('elevenlabsEnableKeyterms', value)
})

const recognitionModeOptions = computed(() => buildElevenLabsRecognitionModeOptions(t))
const postRecordingRefineOptions = computed(() => buildElevenLabsPostRecordingRefineOptions(t))

const batchRefineDisabled = computed(() => elevenlabsRecognitionMode.value === 'batch')
</script>

<template>
  <div class="surface-card asr-card">
    <div class="card-header">
      <div class="card-title">{{ t('asr.elevenlabsConfiguration') }}</div>
      <div class="card-sub">{{ t('asr.elevenlabsConfigurationSub') }}</div>
    </div>
    <div class="field-list">
      <div class="field-row">
        <div class="field-text">
          <div class="field-label">{{ t('asr.apiCredentials') }}</div>
          <div class="field-note">{{ t('asr.elevenlabsApiKeyNote') }}</div>
        </div>
        <NInput
          v-model:value="elevenlabsApiKey"
          type="password"
          show-password-on="click"
          placeholder="sk_..."
          class="field-control"
        />
      </div>
      <div class="field-row">
        <div class="field-text">
          <div class="field-label">{{ t('asr.recognitionMode') }}</div>
          <div class="field-note">{{ t('asr.elevenlabsRecognitionModeNote') }}</div>
        </div>
        <NSelect
          v-model:value="elevenlabsRecognitionMode"
          :options="recognitionModeOptions"
          size="small"
          class="field-control"
        />
      </div>
      <div class="field-row">
        <div class="field-text">
          <div class="field-label">{{ t('asr.postRecordingRefine') }}</div>
          <div class="field-note">{{ t('asr.elevenlabsPostRecordingRefineNote') }}</div>
        </div>
        <NSelect
          v-model:value="elevenlabsPostRecordingRefine"
          :options="postRecordingRefineOptions"
          :disabled="batchRefineDisabled"
          size="small"
          class="field-control"
        />
      </div>
      <div class="field-row">
        <div class="field-text">
          <div class="field-label">{{ t('asr.elevenlabsRealtimeModel') }}</div>
          <div class="field-note">{{ t('asr.elevenlabsRealtimeModelNote') }}</div>
        </div>
        <NSelect
          v-model:value="elevenlabsRealtimeModel"
          :options="ELEVENLABS_REALTIME_MODEL_OPTIONS"
          size="small"
          class="field-control"
        />
      </div>
      <div class="field-row">
        <div class="field-text">
          <div class="field-label">{{ t('asr.elevenlabsBatchModel') }}</div>
          <div class="field-note">{{ t('asr.elevenlabsBatchModelNote') }}</div>
        </div>
        <NSelect
          v-model:value="elevenlabsBatchModel"
          :options="ELEVENLABS_BATCH_MODEL_OPTIONS"
          size="small"
          class="field-control"
        />
      </div>
      <div class="field-row">
        <div class="field-text">
          <div class="field-label">{{ t('asr.languageHint') }}</div>
          <div class="field-note">{{ t('asr.elevenlabsLanguageNote') }}</div>
        </div>
        <NInput
          v-model:value="elevenlabsLanguage"
          :placeholder="t('asr.elevenlabsLanguagePlaceholder')"
          class="field-control"
        />
      </div>
      <div class="field-row">
        <div class="field-text">
          <div class="field-label">{{ t('asr.elevenlabsEnableKeyterms') }}</div>
          <div class="field-note">{{ t('asr.elevenlabsEnableKeytermsNote') }}</div>
        </div>
        <NCheckbox v-model:checked="elevenlabsEnableKeyterms" />
      </div>
    </div>
  </div>
</template>

<style scoped>
@import '../../styles/asr-settings.css';
</style>
