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
  <div class="surface-card asr-card">
    <div class="card-header">
      <div class="card-title">{{ t('asr.qwenRealtimeConfiguration') }}</div>
      <div class="card-sub">{{ t('asr.qwenRealtimeConfigurationSub') }}</div>
    </div>
    <section class="qwen-section" aria-labelledby="qwen-recognition-heading">
      <h3 id="qwen-recognition-heading" class="qwen-section-title">{{ t('asr.qwenRecognitionSection') }}</h3>
      <div class="field-list">
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
        <div v-if="qwenAsrRecognitionMode === 'realtime'" class="field-row">
          <div class="field-text">
            <div class="field-label">{{ t('asr.qwenRealtimeModel') }}</div>
            <div class="field-note">{{ t('asr.modelNote') }}</div>
          </div>
          <AsrModelSelect v-model:value="qwenAsrModel" provider="qwen" mode="realtime" class="field-control" />
        </div>
        <div v-if="qwenAsrRecognitionMode === 'realtime'" class="field-row">
          <div class="field-text">
            <div class="field-label">{{ t('asr.postRecordingRefine') }}</div>
            <div class="field-note">{{ t('asr.qwenPostRecordingRefineNote') }}</div>
          </div>
          <NSelect
            v-model:value="qwenAsrPostRecordingRefine"
            :options="postRecordingRefineOptions"
            size="small"
            class="field-control"
          />
        </div>
        <div v-if="usesBatchModel" class="field-row">
          <div class="field-text">
            <div class="field-label">{{ t('asr.qwenBatchModel') }}</div>
            <div class="field-note">{{ t('asr.qwenBatchModelNote') }}</div>
          </div>
          <AsrModelSelect v-model:value="qwenAsrBatchModel" provider="qwen" mode="batch" class="field-control" />
        </div>
        <div v-if="showQwenRecordingLimitNotice" class="notice-box">
          {{ t('asr.qwenRecordingLimitNotice', {
            minutes: qwenRecordingHardLimitMinutes ?? QWEN_BATCH_RECORDING_LIMIT_MINUTES
          }) }}
        </div>
      </div>
    </section>

    <section class="qwen-section" aria-labelledby="qwen-connection-heading">
      <h3 id="qwen-connection-heading" class="qwen-section-title">{{ t('asr.qwenConnectionSection') }}</h3>
      <div class="field-list">
        <div class="field-row">
          <div class="field-text">
            <div class="field-label">{{ t('asr.qwenServiceRegion') }}</div>
            <div class="field-note">{{ t('asr.qwenServiceRegionNote') }}</div>
          </div>
          <NSelect v-model:value="qwenAsrRegion" :options="qwenRegionOptions" size="small" class="field-control" />
        </div>
        <div v-if="qwenAsrRegion === 'custom'" class="field-row">
          <div class="field-text">
            <div class="field-label">{{ t('asr.endpoint') }}</div>
            <div class="field-note">{{ t('asr.qwenCustomEndpointNote') }}</div>
          </div>
          <NInput v-model:value="qwenAsrWsUrl" placeholder="wss://..." class="field-control"
            :status="connection.error === 'endpoint' ? 'error' : undefined" />
        </div>
        <div v-if="usesQwenAudioFeatures && !connection.workspaceEmbedded" class="field-row">
          <div class="field-text">
            <div class="field-label">{{ t('asr.qwenWorkspaceId') }}</div>
            <div class="field-note">{{ t('asr.qwenWorkspaceIdNote') }}</div>
          </div>
          <NInput
            v-model:value="qwenAsrWorkspaceId"
            :placeholder="t('asr.qwenWorkspaceIdPlaceholder')"
            :status="connection.error === 'workspaceInvalid' ? 'error' : undefined"
            class="field-control"
          />
        </div>
        <div v-if="usesQwenAudioFeatures && connection.workspaceEmbedded" class="field-note">
          {{ t('asr.qwenWorkspaceFromEndpoint') }}
        </div>
        <NAlert v-if="connection.error" type="warning" class="field-alert">
          {{ connectionErrorText }}
        </NAlert>
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
        <div v-if="connection.endpoints.length" class="endpoint-preview" aria-live="polite">
          <div class="endpoint-preview-title">{{ t('asr.qwenResolvedEndpoints') }}</div>
          <div v-for="endpoint in connection.endpoints" :key="endpoint.mode" class="endpoint-preview-row">
            <span>{{ t(endpoint.mode === 'realtime' ? 'asr.qwenRealtimeEndpoint' : 'asr.qwenBatchEndpoint') }}</span>
            <code>{{ endpoint.url }}</code>
          </div>
        </div>
      </div>
    </section>

    <section class="qwen-section" aria-labelledby="qwen-options-heading">
      <h3 id="qwen-options-heading" class="qwen-section-title">{{ t('asr.qwenOptionsSection') }}</h3>
      <div class="field-list">
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
      </div>
    </section>
  </div>
</template>

<style scoped>
@import '../../styles/asr-settings.css';

.qwen-section + .qwen-section {
  margin-top: var(--spacing-xl);
  padding-top: var(--spacing-lg);
  border-top: 1px solid var(--color-divider);
}

.qwen-section-title {
  margin: 0 0 var(--spacing-md);
  color: var(--color-text-secondary);
  font-size: var(--font-sm);
  font-weight: 600;
}

.endpoint-preview {
  padding-top: var(--spacing-sm);
  color: var(--color-text-tertiary);
  font-size: var(--font-xs);
}

.endpoint-preview-title {
  margin-bottom: 6px;
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

@media (max-width: 760px) {
  .field-row {
    flex-direction: column;
    align-items: stretch;
    gap: var(--spacing-sm);
  }

  .field-control {
    width: 100%;
  }

  .field-row > .n-switch {
    align-self: flex-start;
  }

  .endpoint-preview-row {
    flex-direction: column;
    gap: 2px;
  }
}

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
