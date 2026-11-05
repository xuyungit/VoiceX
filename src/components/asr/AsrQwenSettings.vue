<script setup lang="ts">
import { computed, ref, watch } from 'vue'
import AsrModelSelect from './AsrModelSelect.vue'
import { NAlert, NInput, NInputNumber, NSelect, NSwitch, NTag } from 'naive-ui'
import { useI18n } from 'vue-i18n'
import { useSettingsStore } from '../../stores/settings'
import {
  QWEN_ASR_REGIONS,
  isQwenAudioStreamingModel,
  qwenAsrPresetEndpoint,
  qwenAsrRegionFromEndpoint,
  resolveQwenAsrConnection,
  type QwenAsrRegion,
} from '../../utils/qwenAsrSettings'
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
import SettingsSection from '../settings/SettingsSection.vue'
import SettingRow from '../settings/SettingRow.vue'

const settingsStore = useSettingsStore()
const { t } = useI18n()
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
  set: (v: string) => settingsStore.updateSetting('qwenAsrModel', v)
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



const customEndpointSelected = ref(qwenAsrRegionFromEndpoint(qwenAsrWsUrl.value) === 'custom')
const qwenAsrRegion = computed({
  get: () => customEndpointSelected.value ? 'custom' : qwenAsrRegionFromEndpoint(qwenAsrWsUrl.value),
  set: (region: QwenAsrRegion | 'custom') => {
    customEndpointSelected.value = region === 'custom'
    if (region !== 'custom') qwenAsrWsUrl.value = qwenAsrPresetEndpoint(region, qwenAsrModel.value)
  }
})
const qwenRegionOptions = computed(() => [
  ...QWEN_ASR_REGIONS.map(region => ({ label: t(region.labelKey), value: region.value })),
  { label: t('asr.qwenCustomEndpoint'), value: 'custom' },
])

// Only normalize managed presets. Custom hosts, paths and query strings stay intact.
watch(qwenAsrModel, model => {
  const region = qwenAsrRegionFromEndpoint(qwenAsrWsUrl.value)
  if (!customEndpointSelected.value && region !== 'custom') {
    qwenAsrWsUrl.value = qwenAsrPresetEndpoint(region, model)
  }
}, { immediate: true, flush: 'sync' })

const connection = computed(() => resolveQwenAsrConnection({
  recognitionMode: qwenAsrRecognitionMode.value,
  model: qwenAsrModel.value,
  batchModel: qwenAsrBatchModel.value,
  postRecordingRefine: settingsStore.settings.qwenAsrPostRecordingRefine,
  endpoint: qwenAsrWsUrl.value,
  workspaceId: qwenAsrWorkspaceId.value,
}))
const connectionErrorText = computed(() => connection.value.error
  ? t({
    endpoint: 'asr.qwenEndpointInvalid',
    workspaceRequired: 'asr.qwenWorkspaceRequiredBody',
    workspaceInvalid: 'asr.qwenWorkspaceIdInvalid',
  }[connection.value.error]) : '')
const usesBatchModel = computed(() =>
  qwenAsrRecognitionMode.value === 'batch' || settingsStore.settings.qwenAsrPostRecordingRefine
)

const hotwordWeightOptions = computed(() => [1, 2, 3, 4, 5, 50].map(value => ({
  label: value === 50 ? t('asr.qwenSuperHotwordWeight') : String(value),
  value
})))

const usesActiveQwenAudioStreaming = computed(() =>
  qwenAsrRecognitionMode.value === 'realtime' && isQwenAudioStreamingModel(qwenAsrModel.value)
)
const usesQwenAudioFeatures = computed(() => connection.value.needsWorkspace)
const hasDictionary = computed(() => settingsStore.settings.dictionaryText.trim().length > 0)
const vocabularyIdOverridden = computed(() =>
  usesQwenAudioFeatures.value && hasDictionary.value && qwenAsrVocabularyId.value.trim().length > 0
)

const recognitionModeOptions = computed(() => buildBatchCapableRecognitionModeOptions(t))
const postRecordingRefineOptions = computed(() => buildPostRecordingBatchRefineOptions(t))
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
  <SettingsSection :title="t('asr.qwenRealtimeConfiguration')">
    <h3 class="settings-subhead">{{ t('asr.qwenRecognitionSection') }}</h3>
    <SettingRow :label="t('asr.recognitionMode')" :help="t('asr.qwenRecognitionModeNote')">
      <NSelect
        v-model:value="qwenAsrRecognitionMode"
        :options="recognitionModeOptions"
        size="small"
      />
    </SettingRow>
    <SettingRow v-if="qwenAsrRecognitionMode === 'realtime'" :label="t('asr.qwenRealtimeModel')" :help="t('asr.modelNote')">
      <AsrModelSelect v-model:value="qwenAsrModel" provider="qwen" mode="realtime" />
    </SettingRow>
    <SettingRow v-if="qwenAsrRecognitionMode === 'realtime'" :label="t('asr.postRecordingRefine')" :help="t('asr.qwenPostRecordingRefineNote')">
      <NSelect
        v-model:value="qwenAsrPostRecordingRefine"
        :options="postRecordingRefineOptions"
        size="small"
      />
    </SettingRow>
    <SettingRow v-if="usesBatchModel" :label="t('asr.qwenBatchModel')" :help="t('asr.qwenBatchModelNote')">
      <AsrModelSelect v-model:value="qwenAsrBatchModel" provider="qwen" mode="batch" />
    </SettingRow>
    <div v-if="showQwenRecordingLimitNotice" class="settings-callout">
      {{ t('asr.qwenRecordingLimitNotice', {
        minutes: qwenRecordingHardLimitMinutes ?? QWEN_BATCH_RECORDING_LIMIT_MINUTES
      }) }}
    </div>
    <h3 class="settings-subhead">{{ t('asr.qwenConnectionSection') }}</h3>
    <SettingRow :label="t('asr.qwenServiceRegion')" :help="t('asr.qwenServiceRegionNote')">
      <NSelect v-model:value="qwenAsrRegion" :options="qwenRegionOptions" size="small" />
    </SettingRow>
    <SettingRow v-if="qwenAsrRegion === 'custom'" :label="t('asr.endpoint')" :help="t('asr.qwenCustomEndpointNote')">
      <NInput v-model:value="qwenAsrWsUrl" placeholder="wss://..."
        :status="connection.error === 'endpoint' ? 'error' : undefined" />
    </SettingRow>
    <SettingRow v-if="usesQwenAudioFeatures && !connection.workspaceEmbedded" :label="t('asr.qwenWorkspaceId')" :help="t('asr.qwenWorkspaceIdNote')">
      <NInput
        v-model:value="qwenAsrWorkspaceId"
        :placeholder="t('asr.qwenWorkspaceIdPlaceholder')"
        :status="connection.error === 'workspaceInvalid' ? 'error' : undefined"
      />
    </SettingRow>
    <SettingRow v-if="usesQwenAudioFeatures && connection.workspaceEmbedded" :label="t('asr.qwenWorkspaceId')">
      <template #status>
        <span>{{ t('asr.qwenWorkspaceFromEndpoint') }}</span>
      </template>
    </SettingRow>
    <NAlert v-if="connection.error" type="warning">
      {{ connectionErrorText }}
    </NAlert>
    <SettingRow :label="t('asr.apiCredentials')" :help="t('asr.qwenApiKeyNote')">
      <NInput
        v-model:value="qwenAsrApiKey"
        type="password"
        show-password-on="click"
        placeholder="sk-..."
      />
    </SettingRow>
    <div v-if="connection.endpoints.length" class="endpoint-preview" aria-live="polite">
      <div class="endpoint-preview-title">{{ t('asr.qwenResolvedEndpoints') }}</div>
      <div v-for="endpoint in connection.endpoints" :key="endpoint.mode" class="endpoint-preview-row">
        <span>{{ t(endpoint.mode === 'realtime' ? 'asr.qwenRealtimeEndpoint' : 'asr.qwenBatchEndpoint') }}</span>
        <code>{{ endpoint.url }}</code>
      </div>
    </div>
    <h3 class="settings-subhead">{{ t('asr.qwenOptionsSection') }}</h3>
    <SettingRow v-if="usesQwenAudioFeatures" :label="t('asr.qwenAudio3Capabilities')" :help="t('asr.qwenAudio3CapabilitiesNote')">
      <div class="capability-tags">
        <NTag type="success" size="small" :bordered="false">{{ t('asr.qwenCapabilityInstantHotword') }}</NTag>
        <NTag type="success" size="small" :bordered="false">{{ t('asr.qwenCapabilityContext') }}</NTag>
      </div>
    </SettingRow>
    <SettingRow :label="t('asr.languageHint')" :help="t('asr.languageHintNote')">
      <NInput
        v-model:value="qwenAsrLanguage"
        :placeholder="t('asr.qwenLanguagePlaceholder')"
      />
    </SettingRow>
    <SettingRow v-if="usesActiveQwenAudioStreaming" :label="t('asr.qwenContextHistory')" :help="t('asr.qwenContextHistoryNote')">
      <NSwitch v-model:value="enableAsrContext" />
    </SettingRow>
    <SettingRow v-if="usesQwenAudioFeatures" :label="t('asr.qwenHotwordWeight')" :help="t('asr.qwenHotwordWeightNote')">
      <NSelect
        v-model:value="qwenAsrHotwordWeight"
        :options="hotwordWeightOptions"
        size="small"
      />
    </SettingRow>
    <SettingRow v-if="usesQwenAudioFeatures" :label="t('asr.qwenVocabularyId')" :help="t('asr.qwenVocabularyIdNote')">
      <NInput
        v-model:value="qwenAsrVocabularyId"
        :placeholder="t('asr.qwenVocabularyIdPlaceholder')"
      />
    </SettingRow>
    <NAlert
      v-if="vocabularyIdOverridden"
      type="warning"
      :title="t('asr.qwenVocabularyOverrideTitle')"
     
    >
      {{ t('asr.qwenVocabularyOverrideBody') }}
    </NAlert>
    <SettingRow v-if="usesActiveQwenAudioStreaming" :label="t('asr.qwenSemanticPunctuation')" :help="t('asr.qwenSemanticPunctuationNote')">
      <NSwitch v-model:value="qwenAsrSemanticPunctuationEnabled" />
    </SettingRow>
    <SettingRow v-if="usesActiveQwenAudioStreaming" :label="t('asr.qwenSentenceSilence')" :help="t('asr.qwenSentenceSilenceNote')">
      <NInputNumber
        v-model:value="qwenAsrMaxSentenceSilenceMs"
        :min="200"
        :max="6000"
        :step="100"
        size="small"
      />
    </SettingRow>
    <SettingRow v-if="usesActiveQwenAudioStreaming" :label="t('asr.qwenHeartbeat')" :help="t('asr.qwenHeartbeatNote')">
      <NSwitch v-model:value="qwenAsrHeartbeat" />
    </SettingRow>
  </SettingsSection>
</template>

<style scoped>
@import '../../styles/asr-settings.css';

.endpoint-preview {
  color: var(--color-text-tertiary);
  font-size: var(--font-xs);
  user-select: text;
}

.endpoint-preview-title {
  margin-bottom: 4px;
}

.endpoint-preview-row {
  display: flex;
  align-items: baseline;
  gap: var(--spacing-md);
  line-height: 1.6;
}

.endpoint-preview-row span {
  flex: 0 0 auto;
  color: var(--color-text-secondary);
}

.endpoint-preview-row code {
  min-width: 0;
  overflow-wrap: anywhere;
}
</style>
