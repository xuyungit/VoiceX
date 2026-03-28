<script setup lang="ts">
import { computed } from 'vue'
import { NInput, NInputNumber, NSelect } from 'naive-ui'
import { useI18n } from 'vue-i18n'
import { useSettingsStore } from '../../stores/settings'

const settingsStore = useSettingsStore()
const { t } = useI18n()

const googleSttApiKey = computed({
  get: () => settingsStore.settings.googleSttApiKey,
  set: (v: string) => settingsStore.updateSetting('googleSttApiKey', v)
})

const googleSttProjectId = computed({
  get: () => settingsStore.settings.googleSttProjectId,
  set: (v: string) => settingsStore.updateSetting('googleSttProjectId', v)
})

const googleSttLocation = computed({
  get: () => settingsStore.settings.googleSttLocation,
  set: (v: string) => settingsStore.updateSetting('googleSttLocation', v)
})

const googleSttLanguageCode = computed({
  get: () => settingsStore.settings.googleSttLanguageCode,
  set: (v: string) => settingsStore.updateSetting('googleSttLanguageCode', v)
})

const googleSttEndpointing = computed({
  get: () => settingsStore.settings.googleSttEndpointing,
  set: (v: 'supershort' | 'short' | 'standard') => settingsStore.updateSetting('googleSttEndpointing', v)
})

const googleSttPhraseBoost = computed({
  get: () => settingsStore.settings.googleSttPhraseBoost,
  set: (v: number | null) => settingsStore.updateSetting('googleSttPhraseBoost', v ?? 0)
})

const googleEndpointingOptions = computed(() => [
  { label: t('asr.endpointingSupershort'), value: 'supershort' },
  { label: t('asr.endpointingShort'), value: 'short' },
  { label: t('asr.endpointingStandard'), value: 'standard' },
])

const googleLocationOptions = [
  { label: 'us (Multi-region)', value: 'us' },
  { label: 'eu (Multi-region)', value: 'eu' },
  { label: 'asia-southeast1 (Singapore)', value: 'asia-southeast1' },
  { label: 'asia-northeast1 (Tokyo)', value: 'asia-northeast1' },
  { label: 'asia-south1 (Mumbai) [Preview]', value: 'asia-south1' },
  { label: 'europe-west2 (London) [Preview]', value: 'europe-west2' },
  { label: 'europe-west3 (Frankfurt) [Preview]', value: 'europe-west3' },
  { label: 'northamerica-northeast1 (Montreal) [Preview]', value: 'northamerica-northeast1' },
]
</script>

<template>
  <div class="surface-card asr-card">
    <div class="card-header">
      <div class="card-title">{{ t('asr.googleCloudConfiguration') }}</div>
      <div class="card-sub">{{ t('asr.googleCloudConfigurationSub') }}</div>
    </div>
    <div class="field-list">
      <div class="field-row sa-json-row">
        <div class="field-text">
          <div class="field-label">{{ t('asr.serviceAccountKey') }}</div>
          <div class="field-note">{{ t('asr.serviceAccountNote') }}</div>
        </div>
        <NInput
          v-model:value="googleSttApiKey"
          type="textarea"
          placeholder='{"type":"service_account","project_id":"...","private_key":"...",...}'
          :autosize="{ minRows: 3, maxRows: 6 }"
          class="field-control"
        />
      </div>
      <div class="field-row">
        <div class="field-text">
          <div class="field-label">{{ t('asr.projectId') }}</div>
          <div class="field-note">{{ t('asr.projectIdNote') }}</div>
        </div>
        <NInput v-model:value="googleSttProjectId" placeholder="e.g. my-project-123456" class="field-control" />
      </div>
      <div class="field-row">
        <div class="field-text">
          <div class="field-label">{{ t('asr.location') }}</div>
          <div class="field-note">{{ t('asr.locationNote') }}</div>
        </div>
        <NSelect
          v-model:value="googleSttLocation"
          :options="googleLocationOptions"
          filterable
          tag
          size="small"
          class="field-control"
        />
      </div>
      <div class="field-row">
        <div class="field-text">
          <div class="field-label">{{ t('asr.language') }}</div>
          <div class="field-note">{{ t('asr.languageNote') }}</div>
        </div>
        <NInput
          v-model:value="googleSttLanguageCode"
          placeholder="cmn-Hans-CN, en-US"
          class="field-control"
        />
      </div>
      <div class="field-row">
        <div class="field-text">
          <div class="field-label">{{ t('asr.endpointing') }}</div>
          <div class="field-note">{{ t('asr.endpointingNote') }}</div>
        </div>
        <NSelect
          v-model:value="googleSttEndpointing"
          :options="googleEndpointingOptions"
          size="small"
          class="field-control"
        />
      </div>
      <div class="field-row">
        <div class="field-text">
          <div class="field-label">{{ t('asr.phraseBoost') }}</div>
          <div class="field-note">{{ t('asr.phraseBoostNote') }}</div>
        </div>
        <NInputNumber
          v-model:value="googleSttPhraseBoost"
          :min="0"
          :max="20"
          :step="1"
          class="field-control short"
        />
      </div>
    </div>
  </div>
</template>

<style scoped>
@import '../../styles/asr-settings.css';
</style>
