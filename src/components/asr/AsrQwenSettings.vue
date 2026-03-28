<script setup lang="ts">
import { computed } from 'vue'
import { NInput, NSelect } from 'naive-ui'
import { useI18n } from 'vue-i18n'
import { useSettingsStore } from '../../stores/settings'

const settingsStore = useSettingsStore()
const { t } = useI18n()

const qwenAsrApiKey = computed({
  get: () => settingsStore.settings.qwenAsrApiKey,
  set: (v: string) => settingsStore.updateSetting('qwenAsrApiKey', v)
})

const qwenAsrModel = computed({
  get: () => settingsStore.settings.qwenAsrModel,
  set: (v: string) => settingsStore.updateSetting('qwenAsrModel', v)
})

const qwenAsrWsUrl = computed({
  get: () => settingsStore.settings.qwenAsrWsUrl,
  set: (v: string) => settingsStore.updateSetting('qwenAsrWsUrl', v)
})

const qwenAsrLanguage = computed({
  get: () => settingsStore.settings.qwenAsrLanguage,
  set: (v: string) => settingsStore.updateSetting('qwenAsrLanguage', v)
})

const qwenModelOptions = computed(() => [
  { label: t('asr.qwenModelStable'), value: 'qwen3-asr-flash-realtime' },
  { label: t('asr.qwenModelSnapshot1'), value: 'qwen3-asr-flash-realtime-2026-02-10' },
  { label: t('asr.qwenModelSnapshot2'), value: 'qwen3-asr-flash-realtime-2025-10-27' },
])

const qwenWsUrlOptions = computed(() => [
  { label: t('asr.qwenEndpointBeijing'), value: 'wss://dashscope.aliyuncs.com/api-ws/v1/realtime' },
  { label: t('asr.qwenEndpointSingapore'), value: 'wss://dashscope-intl.aliyuncs.com/api-ws/v1/realtime' },
])
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
    </div>
  </div>
</template>

<style scoped>
@import '../../styles/asr-settings.css';
</style>
