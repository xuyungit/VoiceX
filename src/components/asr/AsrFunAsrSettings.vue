<script setup lang="ts">
import { computed } from 'vue'
import { findAsrModel } from '../../utils/asrModels'
import AsrModelSelect from './AsrModelSelect.vue'
import { NInput, NSelect, NTag, NAlert } from 'naive-ui'
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

// 用户词典是否非空（全局配置，来自 Dictionary.vue）
const hasDictionary = computed(() => {
  const text = settingsStore.settings.dictionaryText || ''
  return text.split(/[\n,，\s]+/).map(s => s.trim()).filter(Boolean).length > 0
})

const currentModel = computed(() => findAsrModel('funasr', funasrModel.value))

// 当用户填了词典但选中模型不支持上下文增强时，显式提示。
// 遵守 AGENTS.md 防静默失败规则：不能让用户 unknowingly 用了不支持的模型还以为词典生效。
const contextUnsupportedWarning = computed(() => {
  if (!hasDictionary.value) return false
  if (!currentModel.value) return false
  return !currentModel.value.supportsContext
})

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
        <AsrModelSelect v-model:value="funasrModel" provider="funasr" mode="realtime" class="field-control" />
      </div>
      <div v-if="currentModel" class="field-row capability-row">
        <div class="field-text">
          <div class="field-label">{{ t('asr.funasrCapabilityLabel') }}</div>
          <div class="field-note">{{ t('asr.funasrCapabilityNote') }}</div>
        </div>
        <div class="capability-tags">
          <NTag
            :type="currentModel.supportsContext ? 'success' : 'default'"
            size="small"
            :bordered="false"
          >
            {{ t('asr.funasrCapabilityContext') }}: {{ currentModel.supportsContext ? t('asr.funasrSupported') : t('asr.funasrNotSupported') }}
          </NTag>
          <NTag
            :type="currentModel.supportsHotword ? 'success' : 'default'"
            size="small"
            :bordered="false"
          >
            {{ t('asr.funasrCapabilityHotword') }}: {{ currentModel.supportsHotword ? t('asr.funasrSupported') : t('asr.funasrNotSupported') }}
          </NTag>
        </div>
      </div>
      <NAlert
        v-if="contextUnsupportedWarning"
        type="warning"
        :title="t('asr.funasrContextUnsupportedTitle')"
        class="field-alert"
      >
        {{ t('asr.funasrContextUnsupportedBody') }}
      </NAlert>
      <div class="field-row">
        <div class="field-text">
          <div class="field-label">{{ t('asr.languageHint') }}</div>
          <div class="field-note">{{ t('asr.funasrLanguageNote') }}</div>
        </div>
        <NInput
          v-model:value="funasrLanguage"
          :placeholder="t('asr.leaveEmptyToAuto')"
          class="field-control"
        />
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
