<script setup lang="ts">
import { computed } from 'vue'
import AsrModelSelect from './AsrModelSelect.vue'
import { NInput, NSelect } from 'naive-ui'
import { useI18n } from 'vue-i18n'
import { useSettingsStore } from '../../stores/settings'
import {
  buildElevenLabsPostRecordingRefineOptions,
  buildElevenLabsRecognitionModeOptions,
  normalizeBatchCapablePostRecordingRefine
} from '../../utils/providerOptions'
import SettingsSection from '../settings/SettingsSection.vue'
import SettingRow from '../settings/SettingRow.vue'

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

const recognitionModeOptions = computed(() => buildElevenLabsRecognitionModeOptions(t))
const postRecordingRefineOptions = computed(() => buildElevenLabsPostRecordingRefineOptions(t))

const batchRefineDisabled = computed(() => elevenlabsRecognitionMode.value === 'batch')
</script>

<template>
  <SettingsSection :title="t('asr.elevenlabsConfiguration')" :help="t('asr.elevenlabsConfigurationSub')">
    <SettingRow :label="t('asr.apiCredentials')" :help="t('asr.elevenlabsApiKeyNote')">
      <NInput
        v-model:value="elevenlabsApiKey"
        type="password"
        show-password-on="click"
        placeholder="sk_..."
      />
    </SettingRow>
    <SettingRow :label="t('asr.recognitionMode')" :help="t('asr.elevenlabsRecognitionModeNote')">
      <NSelect
        v-model:value="elevenlabsRecognitionMode"
        :options="recognitionModeOptions"
        size="small"
      />
    </SettingRow>
    <SettingRow :label="t('asr.postRecordingRefine')" :help="t('asr.elevenlabsPostRecordingRefineNote')">
      <NSelect
        v-model:value="elevenlabsPostRecordingRefine"
        :options="postRecordingRefineOptions"
        :disabled="batchRefineDisabled"
        size="small"
      />
    </SettingRow>
    <SettingRow :label="t('asr.elevenlabsRealtimeModel')" :help="t('asr.elevenlabsRealtimeModelNote')">
      <AsrModelSelect v-model:value="elevenlabsRealtimeModel" provider="elevenlabs" mode="realtime" />
    </SettingRow>
    <SettingRow :label="t('asr.elevenlabsBatchModel')" :help="t('asr.elevenlabsBatchModelNote')">
      <AsrModelSelect v-model:value="elevenlabsBatchModel" provider="elevenlabs" mode="batch" />
    </SettingRow>
    <SettingRow :label="t('asr.languageHint')" :help="t('asr.elevenlabsLanguageNote')">
      <NInput
        v-model:value="elevenlabsLanguage"
        :placeholder="t('asr.elevenlabsLanguagePlaceholder')"
      />
    </SettingRow>
  </SettingsSection>
</template>

<style scoped>
@import '../../styles/asr-settings.css';
</style>
