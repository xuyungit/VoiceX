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
import SettingsSection from '../settings/SettingsSection.vue'
import SettingRow from '../settings/SettingRow.vue'

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
  <SettingsSection :title="t('asr.openaiConfiguration')">
    <SettingRow :label="t('asr.apiCredentials')">
      <NInput
        v-model:value="openaiAsrApiKey"
        type="password"
        show-password-on="click"
        placeholder="sk-..."
      />
    </SettingRow>
    <SettingRow :label="t('asr.recognitionMode')">
      <NSelect
        v-model:value="openaiAsrMode"
        :options="openaiModeOptions"
        size="small"
      />
    </SettingRow>
    <SettingRow :label="t(openaiAsrMode === 'realtime' ? 'asr.openaiRealtimeModel' : 'asr.openaiBatchModel')">
      <template #status>
        <div v-if="committedRealtime">{{ t('asr.openaiCommittedRealtimeNote') }}</div>
      </template>
      <AsrModelSelect v-model:value="openaiAsrModel" provider="openai" :mode="openaiAsrMode" />
    </SettingRow>
    <SettingRow v-if="showDelay" :label="t('asr.openaiDelay')" :help="t('asr.openaiDelayShortNote')">
      <NSelect v-model:value="openaiAsrDelay" :options="openaiDelayOptions" size="small" />
    </SettingRow>
    <SettingRow v-if="openaiAsrMode === 'realtime'" :label="t('asr.postRecordingRefine')" :help="t('asr.openaiRefineShortNote')">
      <NSelect v-model:value="openaiAsrPostRecordingRefine" :options="openaiPostRecordingRefineOptions" size="small" />
    </SettingRow>
    <SettingRow v-if="openaiAsrMode === 'realtime' && openaiAsrPostRecordingRefine === 'batch_refine'" :label="t('asr.refineModel')">
      <AsrModelSelect v-model:value="openaiAsrRefineModel" provider="openai" mode="batch" />
    </SettingRow>
    <SettingRow :label="t('asr.endpoint')">
      <NInput
        v-model:value="openaiAsrBaseUrl"
        placeholder="https://api.openai.com/v1"
      />
    </SettingRow>
    <SettingRow :label="t('asr.languageHint')" :help="t('asr.openaiLanguageShortNote')">
      <NInput
        v-model:value="openaiAsrLanguage"
        placeholder="zh, en"
      />
    </SettingRow>
    <SettingRow stacked :label="t('asr.prompt')" :help="t('asr.openaiPromptShortNote')">
      <NInput
        v-model:value="openaiAsrPrompt"
        type="textarea"
        :autosize="{ minRows: 3, maxRows: 6 }"
      />
    </SettingRow>
  </SettingsSection>
</template>

<style scoped>
@import '../../styles/asr-settings.css';
</style>
