<script setup lang="ts">
import { computed } from 'vue'
import AsrModelSelect from './AsrModelSelect.vue'
import { findAsrModel } from '../../utils/asrModels'
import { NInput, NSelect } from 'naive-ui'
import { useI18n } from 'vue-i18n'
import { useSettingsStore } from '../../stores/settings'
import { buildBatchCapableRecognitionModeOptions, buildPostRecordingBatchRefineOptions, normalizeBatchCapablePostRecordingRefine } from '../../utils/providerOptions'
import SettingsSection from '../settings/SettingsSection.vue'
import SettingRow from '../settings/SettingRow.vue'

const settingsStore = useSettingsStore()
const { t } = useI18n()

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

const recognitionMode = computed({
  get: () => settingsStore.settings.geminiRecognitionMode,
  set: (v: 'batch' | 'realtime') => {
    settingsStore.updateSetting('geminiRecognitionMode', v)
    settingsStore.updateSetting('geminiPostRecordingRefine', normalizeBatchCapablePostRecordingRefine(v, settingsStore.settings.geminiPostRecordingRefine))
  }
})
const postRecordingRefine = computed({
  get: () => settingsStore.settings.geminiPostRecordingRefine,
  set: (v: 'off' | 'batch_refine') => settingsStore.updateSetting('geminiPostRecordingRefine', v)
})
const modeOptions = computed(() => buildBatchCapableRecognitionModeOptions(t))
const refineOptions = computed(() => buildPostRecordingBatchRefineOptions(t))
const liveModelId = computed(() => findAsrModel('gemini-live', geminiLiveModel.value)?.id)
const batchModelId = computed(() => findAsrModel('gemini', geminiModel.value)?.id)
const liveModelNote = computed(() => liveModelId.value === 'gemini-3.5-transcribe-live'
  ? 'asr.geminiDedicatedLiveNote'
  : liveModelId.value === 'gemini-3.8-live' ? 'asr.geminiAudioLiveNote' : 'asr.geminiCustomLiveNote')
const batchModelNote = computed(() => batchModelId.value === 'gemini-3.5-transcribe'
  ? 'asr.geminiDedicatedBatchNote' : 'asr.geminiGeneralBatchNote')
const languageNote = computed(() => {
  if (recognitionMode.value === 'realtime') {
    return liveModelId.value === 'gemini-3.8-live' ? 'asr.geminiAudioLanguageNote' : 'asr.geminiLiveLanguageNote'
  }
  return batchModelId.value === 'gemini-3.5-transcribe' ? 'asr.geminiTranscribeLanguageNote' : 'asr.geminiLanguageNote'
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
</script>

<template>
  <SettingsSection :title="t('asr.geminiConfiguration')">
    <SettingRow :label="t('asr.apiCredentials')" :help="t('asr.geminiApiKeyNote')">
      <NInput
        v-model:value="geminiApiKey"
        type="password"
        show-password-on="click"
        placeholder="AIza..."
      />
    </SettingRow>
    <SettingRow :label="t('asr.recognitionMode')">
      <NSelect v-model:value="recognitionMode" :options="modeOptions" size="small" />
    </SettingRow>
    <SettingRow v-if="recognitionMode === 'realtime'" :label="t('asr.openaiRealtimeModel')" :help="t(liveModelNote)">
      <AsrModelSelect v-model:value="geminiLiveModel" provider="gemini-live" mode="realtime" />
    </SettingRow>
    <SettingRow v-if="recognitionMode === 'realtime'" :label="t('asr.postRecordingRefine')" :help="t('asr.geminiRefineNote')">
      <NSelect v-model:value="postRecordingRefine" :options="refineOptions" size="small" />
    </SettingRow>
    <SettingRow v-if="recognitionMode === 'batch' || postRecordingRefine === 'batch_refine'" :label="t(recognitionMode === 'batch' ? 'asr.openaiBatchModel' : 'asr.refineModel')" :help="t(batchModelNote)">
      <AsrModelSelect v-model:value="geminiModel" provider="gemini" mode="batch" />
    </SettingRow>
    <SettingRow :label="t('asr.languageHint')" :help="t(languageNote)">
      <NSelect
        v-model:value="geminiLanguage"
        :options="geminiLanguageOptions"
        size="small"
      />
    </SettingRow>
  </SettingsSection>
</template>

<style scoped>
@import '../../styles/asr-settings.css';
</style>
