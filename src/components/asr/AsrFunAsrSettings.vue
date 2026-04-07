<script setup lang="ts">
import { computed } from 'vue'
import { NInput, NSelect } from 'naive-ui'
import { useI18n } from 'vue-i18n'
import { useSettingsStore } from '../../stores/settings'

const settingsStore = useSettingsStore()
const { t } = useI18n()

const funasrApiKey = computed({
  get: () => settingsStore.settings.funasrApiKey,
  set: (v: string) => settingsStore.updateSetting('funasrApiKey', v)
})

const funasrModel = computed({
  get: () => settingsStore.settings.funasrModel,
  set: (v: string) => settingsStore.updateSetting('funasrModel', v)
})

const funasrWsUrl = computed({
  get: () => settingsStore.settings.funasrWsUrl,
  set: (v: string) => settingsStore.updateSetting('funasrWsUrl', v)
})

const funasrLanguage = computed({
  get: () => settingsStore.settings.funasrLanguage,
  set: (v: string) => settingsStore.updateSetting('funasrLanguage', v)
})

const modelOptions = computed(() => [
  { label: t('asr.funasrModelStable'), value: 'fun-asr-realtime' },
  { label: t('asr.funasrModelSnapshot1'), value: 'fun-asr-realtime-2026-02-28' },
  { label: t('asr.funasrModelSnapshot2'), value: 'fun-asr-realtime-2025-11-07' },
  { label: t('asr.funasrModel8kStable'), value: 'fun-asr-flash-8k-realtime' }
])

const endpointOptions = computed(() => [
  { label: t('asr.funasrEndpointBeijing'), value: 'wss://dashscope.aliyuncs.com/api-ws/v1/inference' },
  { label: t('asr.funasrEndpointSingapore'), value: 'wss://dashscope-intl.aliyuncs.com/api-ws/v1/inference' },
])
</script>

<template>
  <div class="surface-card asr-card">
    <div class="card-header">
      <div class="card-title">{{ t('asr.funasrConfiguration') }}</div>
      <div class="card-sub">{{ t('asr.funasrConfigurationSub') }}</div>
    </div>
    <div class="field-list">
      <div class="field-row">
        <div class="field-text">
          <div class="field-label">{{ t('asr.apiCredentials') }}</div>
          <div class="field-note">{{ t('asr.funasrApiKeyNote') }}</div>
        </div>
        <NInput
          v-model:value="funasrApiKey"
          type="password"
          show-password-on="click"
          placeholder="sk-..."
          class="field-control"
        />
      </div>
      <div class="field-row">
        <div class="field-text">
          <div class="field-label">{{ t('asr.endpoint') }}</div>
          <div class="field-note">{{ t('asr.funasrEndpointNote') }}</div>
        </div>
        <NSelect
          v-model:value="funasrWsUrl"
          :options="endpointOptions"
          filterable
          tag
          size="small"
          class="field-control"
        />
      </div>
      <div class="field-row">
        <div class="field-text">
          <div class="field-label">{{ t('asr.model') }}</div>
          <div class="field-note">{{ t('asr.funasrModelNote') }}</div>
        </div>
        <NSelect
          v-model:value="funasrModel"
          :options="modelOptions"
          filterable
          tag
          size="small"
          class="field-control"
        />
      </div>
      <div class="field-row">
        <div class="field-text">
          <div class="field-label">{{ t('asr.languageHint') }}</div>
          <div class="field-note">{{ t('asr.funasrLanguageNote') }}</div>
        </div>
        <NInput
          v-model:value="funasrLanguage"
          placeholder="留空为自动检测"
          class="field-control"
        />
      </div>
    </div>
  </div>
</template>

<style scoped>
@import '../../styles/asr-settings.css';
</style>
