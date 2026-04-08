<script setup lang="ts">
import { computed } from 'vue'
import { NInput, NSelect } from 'naive-ui'
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

const qwenModelOptions = computed(() => [
  { label: t('asr.qwenModelStable'), value: 'qwen3-asr-flash-realtime' },
  { label: t('asr.qwenModelSnapshot1'), value: 'qwen3-asr-flash-realtime-2026-02-10' },
  { label: t('asr.qwenModelSnapshot2'), value: 'qwen3-asr-flash-realtime-2025-10-27' },
])

const qwenBatchModelOptions = computed(() => [
  { label: t('asr.qwenBatchModelStable'), value: 'qwen3-asr-flash' },
  { label: t('asr.qwenBatchModelSnapshot1'), value: 'qwen3-asr-flash-2025-09-08' },
])

const qwenWsUrlOptions = computed(() => [
  { label: t('asr.qwenEndpointBeijing'), value: 'wss://dashscope.aliyuncs.com/api-ws/v1/realtime' },
  { label: t('asr.qwenEndpointSingapore'), value: 'wss://dashscope-intl.aliyuncs.com/api-ws/v1/realtime' },
])

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
          <div class="field-note">填写对应地域的 DashScope API Key。</div>
        </div>
        <NInput
          v-model:value="qwenAsrApiKey"
          type="password"
          show-password-on="click"
          placeholder="sk-..."
          class="field-control"
        />
      </div>
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
        <NSelect
          v-model:value="qwenAsrModel"
          :options="qwenModelOptions"
          filterable
          tag
          size="small"
          class="field-control"
        />
      </div>
      <div class="field-row">
        <div class="field-text">
          <div class="field-label">{{ t('asr.languageHint') }}</div>
          <div class="field-note">{{ t('asr.languageHintNote') }}</div>
        </div>
        <NInput
          v-model:value="qwenAsrLanguage"
          placeholder="留空为自动检测"
          class="field-control"
        />
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
        <NSelect
          v-model:value="qwenAsrBatchModel"
          :options="qwenBatchModelOptions"
          filterable
          tag
          size="small"
          class="field-control"
        />
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
</style>
