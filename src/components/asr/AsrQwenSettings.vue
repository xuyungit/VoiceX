<script setup lang="ts">
import { computed } from 'vue'
import AsrModelSelect from './AsrModelSelect.vue'
import { NAlert, NInput, NInputNumber, NSelect, NSwitch, NTag } from 'naive-ui'
import { useI18n } from 'vue-i18n'
import { useSettingsStore } from '../../stores/settings'
import {
  QWEN_BATCH_RECORDING_LIMIT_MINUTES,
  buildBatchCapableRecognitionModeOptions,
  buildPostRecordingBatchRefineOptions,
  exceedsRecordingHardLimit,
  normalizeBatchCapablePostRecordingRefine,
  postRecordingBatchRefineEnabled,
  postRecordingBatchRefineValueFromBoolean,
  resolveQwenRecordingHardLimitMinutes,
} from '../../utils/providerOptions'

const settingsStore = useSettingsStore()
const { t } = useI18n()
const QWEN_AUDIO_STREAMING_MODEL = 'qwen-audio-3.0-asr-flash-streaming'
const QWEN_AUDIO_BATCH_MODEL = 'qwen-audio-3.0-asr-flash'

const qwenAsrApiKey = computed({
  get: () => settingsStore.settings.qwenAsrApiKey,
  set: (v: string) => settingsStore.updateSetting('qwenAsrApiKey', v)
})

const qwenAsrRecognitionMode = computed({
  get: () => settingsStore.settings.qwenAsrRecognitionMode,
  set: (value: 'realtime' | 'batch') => {
    settingsStore.updateSetting('qwenAsrRecognitionMode', value)
    settingsStore.updateSetting(
      'qwenAsrPostRecordingRefine',
      postRecordingBatchRefineEnabled(
        normalizeBatchCapablePostRecordingRefine(
          value,
          postRecordingBatchRefineValueFromBoolean(settingsStore.settings.qwenAsrPostRecordingRefine)
        )
      )
    )
  }
})

const qwenAsrModel = computed({
  get: () => settingsStore.settings.qwenAsrModel,
  set: (v: string) => {
    settingsStore.updateSetting('qwenAsrModel', v)
    const path = v === QWEN_AUDIO_STREAMING_MODEL ? 'inference' : 'realtime'
    const current = settingsStore.settings.qwenAsrWsUrl
    const next = current.replace(/\/api-ws\/v1\/(?:realtime|inference)(?:\?.*)?$/, `/api-ws/v1/${path}`)
    if (next !== current) settingsStore.updateSetting('qwenAsrWsUrl', next)
  }
})

const qwenAsrWsUrl = computed({
  get: () => settingsStore.settings.qwenAsrWsUrl,
  set: (v: string) => settingsStore.updateSetting('qwenAsrWsUrl', v)
})

const qwenAsrWorkspaceId = computed({
  get: () => settingsStore.settings.qwenAsrWorkspaceId,
  set: (v: string) => settingsStore.updateSetting('qwenAsrWorkspaceId', v)
})

const qwenAsrBatchModel = computed({
  get: () => settingsStore.settings.qwenAsrBatchModel,
  set: (v: string) => settingsStore.updateSetting('qwenAsrBatchModel', v)
})

const qwenAsrLanguage = computed({
  get: () => settingsStore.settings.qwenAsrLanguage,
  set: (v: string) => settingsStore.updateSetting('qwenAsrLanguage', v)
})

const qwenAsrPostRecordingRefine = computed({
  get: (): 'off' | 'batch_refine' =>
    postRecordingBatchRefineValueFromBoolean(settingsStore.settings.qwenAsrPostRecordingRefine),
  set: (value: 'off' | 'batch_refine') => {
    settingsStore.updateSetting(
      'qwenAsrPostRecordingRefine',
      postRecordingBatchRefineEnabled(
        normalizeBatchCapablePostRecordingRefine(qwenAsrRecognitionMode.value, value)
      )
    )
  }
})

const enableAsrContext = computed({
  get: () => settingsStore.settings.enableAsrContext,
  set: (v: boolean) => settingsStore.updateSetting('enableAsrContext', v)
})

const qwenAsrVocabularyId = computed({
  get: () => settingsStore.settings.qwenAsrVocabularyId,
  set: (v: string) => settingsStore.updateSetting('qwenAsrVocabularyId', v)
})

const qwenAsrHotwordWeight = computed({
  get: () => settingsStore.settings.qwenAsrHotwordWeight,
  set: (v: number) => settingsStore.updateSetting('qwenAsrHotwordWeight', v)
})

const qwenAsrSemanticPunctuationEnabled = computed({
  get: () => settingsStore.settings.qwenAsrSemanticPunctuationEnabled,
  set: (v: boolean) => settingsStore.updateSetting('qwenAsrSemanticPunctuationEnabled', v)
})

const qwenAsrMaxSentenceSilenceMs = computed({
  get: () => settingsStore.settings.qwenAsrMaxSentenceSilenceMs,
  set: (v: number | null) => settingsStore.updateSetting('qwenAsrMaxSentenceSilenceMs', v ?? 1300)
})

const qwenAsrHeartbeat = computed({
  get: () => settingsStore.settings.qwenAsrHeartbeat,
  set: (v: boolean) => settingsStore.updateSetting('qwenAsrHeartbeat', v)
})



const qwenWsUrlOptions = computed(() => [
  { label: t('asr.qwenEndpointBeijingInference'), value: 'wss://dashscope.aliyuncs.com/api-ws/v1/inference' },
  { label: t('asr.qwenEndpointSingaporeInference'), value: 'wss://dashscope-intl.aliyuncs.com/api-ws/v1/inference' },
  { label: t('asr.qwenEndpointBeijing'), value: 'wss://dashscope.aliyuncs.com/api-ws/v1/realtime' },
  { label: t('asr.qwenEndpointSingapore'), value: 'wss://dashscope-intl.aliyuncs.com/api-ws/v1/realtime' },
])

const hotwordWeightOptions = computed(() => [1, 2, 3, 4, 5, 50].map(value => ({
  label: value === 50 ? t('asr.qwenSuperHotwordWeight') : String(value),
  value
})))

const usesQwenAudioStreaming = computed(() => qwenAsrModel.value === QWEN_AUDIO_STREAMING_MODEL)
const usesQwenAudioBatch = computed(() => qwenAsrBatchModel.value === QWEN_AUDIO_BATCH_MODEL)
const usesActiveQwenAudioStreaming = computed(() =>
  qwenAsrRecognitionMode.value === 'realtime' && usesQwenAudioStreaming.value
)
const usesActiveQwenAudioBatch = computed(() =>
  usesQwenAudioBatch.value
  && (qwenAsrRecognitionMode.value === 'batch' || settingsStore.settings.qwenAsrPostRecordingRefine)
)
const usesQwenAudioFeatures = computed(() =>
  usesActiveQwenAudioStreaming.value || usesActiveQwenAudioBatch.value
)
const hasQwenAudioModel = computed(() => usesQwenAudioStreaming.value || usesQwenAudioBatch.value)
const hasWorkspaceEndpoint = computed(() =>
  qwenAsrWorkspaceId.value.trim().length > 0
  || /\.maas\.aliyuncs\.com(?:\/|$)/.test(qwenAsrWsUrl.value)
)
const hasDictionary = computed(() => settingsStore.settings.dictionaryText.trim().length > 0)
const vocabularyIdOverridden = computed(() =>
  usesQwenAudioFeatures.value && hasDictionary.value && qwenAsrVocabularyId.value.trim().length > 0
)

const recognitionModeOptions = computed(() => buildBatchCapableRecognitionModeOptions(t))
const postRecordingRefineOptions = computed(() => buildPostRecordingBatchRefineOptions(t))
const batchRefineDisabled = computed(() => qwenAsrRecognitionMode.value === 'batch')
const qwenRecordingHardLimitMinutes = computed(() =>
  resolveQwenRecordingHardLimitMinutes(
    qwenAsrRecognitionMode.value,
    settingsStore.settings.qwenAsrPostRecordingRefine
  )
)
const showQwenRecordingLimitNotice = computed(() =>
  exceedsRecordingHardLimit(
    settingsStore.settings.maxRecordingMinutes,
    qwenRecordingHardLimitMinutes.value
  )
)
</script>

<template>
  <div class="surface-card asr-card">
    <div class="card-header">
      <div class="card-title">{{ t('asr.qwenRealtimeConfiguration') }}</div>
      <div class="card-sub">{{ t('asr.qwenRealtimeConfigurationSub') }}</div>
    </div>
    <div class="field-list">
      <div class="field-row">
        <div class="field-text">
          <div class="field-label">{{ t('asr.apiCredentials') }}</div>
          <div class="field-note">{{ t('asr.qwenApiKeyNote') }}</div>
        </div>
        <NInput
          v-model:value="qwenAsrApiKey"
          type="password"
          show-password-on="click"
          placeholder="sk-..."
          class="field-control"
        />
      </div>
      <div v-if="hasQwenAudioModel" class="field-row">
        <div class="field-text">
          <div class="field-label">{{ t('asr.qwenWorkspaceId') }}</div>
          <div class="field-note">{{ t('asr.qwenWorkspaceIdNote') }}</div>
        </div>
        <NInput
          v-model:value="qwenAsrWorkspaceId"
          :placeholder="t('asr.qwenWorkspaceIdPlaceholder')"
          class="field-control"
        />
      </div>
      <NAlert
        v-if="usesQwenAudioFeatures && !hasWorkspaceEndpoint"
        type="warning"
        :title="t('asr.qwenWorkspaceRequiredTitle')"
        class="field-alert"
      >
        {{ t('asr.qwenWorkspaceRequiredBody') }}
      </NAlert>
      <div class="field-row">
        <div class="field-text">
          <div class="field-label">{{ t('asr.recognitionMode') }}</div>
          <div class="field-note">{{ t('asr.qwenRecognitionModeNote') }}</div>
        </div>
        <NSelect
          v-model:value="qwenAsrRecognitionMode"
          :options="recognitionModeOptions"
          size="small"
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
        <AsrModelSelect v-model:value="qwenAsrModel" provider="qwen" mode="realtime" class="field-control" />
      </div>
      <div v-if="usesQwenAudioFeatures" class="field-row capability-row">
        <div class="field-text">
          <div class="field-label">{{ t('asr.qwenAudio3Capabilities') }}</div>
          <div class="field-note">{{ t('asr.qwenAudio3CapabilitiesNote') }}</div>
        </div>
        <div class="capability-tags">
          <NTag type="success" size="small" :bordered="false">{{ t('asr.qwenCapabilityInstantHotword') }}</NTag>
          <NTag type="success" size="small" :bordered="false">{{ t('asr.qwenCapabilityContext') }}</NTag>
        </div>
      </div>
      <NAlert v-if="usesActiveQwenAudioStreaming" type="info" class="field-alert">
        {{ t('asr.qwenAudio3ProtocolNote') }}
      </NAlert>
      <div class="field-row">
        <div class="field-text">
          <div class="field-label">{{ t('asr.languageHint') }}</div>
          <div class="field-note">{{ t('asr.languageHintNote') }}</div>
        </div>
        <NInput
          v-model:value="qwenAsrLanguage"
          :placeholder="t('asr.qwenLanguagePlaceholder')"
          class="field-control"
        />
      </div>
      <div v-if="usesActiveQwenAudioStreaming" class="field-row">
        <div class="field-text">
          <div class="field-label">{{ t('asr.qwenContextHistory') }}</div>
          <div class="field-note">{{ t('asr.qwenContextHistoryNote') }}</div>
        </div>
        <NSwitch v-model:value="enableAsrContext" />
      </div>
      <div v-if="usesQwenAudioFeatures" class="field-row">
        <div class="field-text">
          <div class="field-label">{{ t('asr.qwenHotwordWeight') }}</div>
          <div class="field-note">{{ t('asr.qwenHotwordWeightNote') }}</div>
        </div>
        <NSelect
          v-model:value="qwenAsrHotwordWeight"
          :options="hotwordWeightOptions"
          size="small"
          class="field-control"
        />
      </div>
      <div v-if="usesQwenAudioFeatures" class="field-row">
        <div class="field-text">
          <div class="field-label">{{ t('asr.qwenVocabularyId') }}</div>
          <div class="field-note">{{ t('asr.qwenVocabularyIdNote') }}</div>
        </div>
        <NInput
          v-model:value="qwenAsrVocabularyId"
          :placeholder="t('asr.qwenVocabularyIdPlaceholder')"
          class="field-control"
        />
      </div>
      <NAlert
        v-if="vocabularyIdOverridden"
        type="warning"
        :title="t('asr.qwenVocabularyOverrideTitle')"
        class="field-alert"
      >
        {{ t('asr.qwenVocabularyOverrideBody') }}
      </NAlert>
      <div v-if="usesActiveQwenAudioStreaming" class="field-row">
        <div class="field-text">
          <div class="field-label">{{ t('asr.qwenSemanticPunctuation') }}</div>
          <div class="field-note">{{ t('asr.qwenSemanticPunctuationNote') }}</div>
        </div>
        <NSwitch v-model:value="qwenAsrSemanticPunctuationEnabled" />
      </div>
      <div v-if="usesActiveQwenAudioStreaming" class="field-row">
        <div class="field-text">
          <div class="field-label">{{ t('asr.qwenSentenceSilence') }}</div>
          <div class="field-note">{{ t('asr.qwenSentenceSilenceNote') }}</div>
        </div>
        <NInputNumber
          v-model:value="qwenAsrMaxSentenceSilenceMs"
          :min="200"
          :max="6000"
          :step="100"
          size="small"
          class="field-control"
        />
      </div>
      <div v-if="usesActiveQwenAudioStreaming" class="field-row">
        <div class="field-text">
          <div class="field-label">{{ t('asr.qwenHeartbeat') }}</div>
          <div class="field-note">{{ t('asr.qwenHeartbeatNote') }}</div>
        </div>
        <NSwitch v-model:value="qwenAsrHeartbeat" />
      </div>
      <div class="field-row">
        <div class="field-text">
          <div class="field-label">{{ t('asr.postRecordingRefine') }}</div>
          <div class="field-note">{{ t('asr.qwenPostRecordingRefineNote') }}</div>
        </div>
        <NSelect
          v-model:value="qwenAsrPostRecordingRefine"
          :options="postRecordingRefineOptions"
          :disabled="batchRefineDisabled"
          size="small"
          class="field-control"
        />
      </div>
      <div class="field-row">
        <div class="field-text">
          <div class="field-label">{{ t('asr.qwenBatchModel') }}</div>
          <div class="field-note">{{ t('asr.qwenBatchModelNote') }}</div>
        </div>
        <AsrModelSelect v-model:value="qwenAsrBatchModel" provider="qwen" mode="batch" class="field-control" />
      </div>
      <div v-if="showQwenRecordingLimitNotice" class="notice-box">
        {{
          t('asr.qwenRecordingLimitNotice', {
            minutes: qwenRecordingHardLimitMinutes ?? QWEN_BATCH_RECORDING_LIMIT_MINUTES
          })
        }}
      </div>
    </div>
  </div>
</template>

<style scoped>
@import '../../styles/asr-settings.css';

.capability-row .capability-tags {
  display: flex;
  flex-wrap: wrap;
  gap: 6px;
  align-items: center;
}

.field-alert {
  margin: 4px 0;
}
</style>
