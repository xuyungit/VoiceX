<script setup lang="ts">
import { computed } from 'vue'
import { findAsrModel } from '../../utils/asrModels'
import { isAsrDictionaryEnabled } from '../../utils/asrDictionary'
import AsrModelSelect from './AsrModelSelect.vue'
import { NInput, NSelect, NTag, NAlert } from 'naive-ui'
import { useI18n } from 'vue-i18n'
import { useSettingsStore } from '../../stores/settings'
import SettingsSection from '../settings/SettingsSection.vue'
import SettingRow from '../settings/SettingRow.vue'

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
  if (!isAsrDictionaryEnabled(settingsStore.settings)) return false
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
  <SettingsSection :title="t('asr.funasrConfiguration')" :help="t('asr.funasrConfigurationSub')">
    <SettingRow :label="t('asr.apiCredentials')" :help="t('asr.funasrApiKeyNote')">
      <NInput
        v-model:value="funasrApiKey"
        type="password"
        show-password-on="click"
        placeholder="sk-..."
      />
    </SettingRow>
    <SettingRow :label="t('asr.endpoint')" :help="t('asr.funasrEndpointNote')">
      <NSelect
        v-model:value="funasrWsUrl"
        :options="endpointOptions"
        filterable
        tag
        size="small"
      />
    </SettingRow>
    <SettingRow :label="t('asr.model')" :help="t('asr.funasrModelNote')">
      <AsrModelSelect v-model:value="funasrModel" provider="funasr" mode="realtime" />
    </SettingRow>
    <SettingRow v-if="currentModel" :label="t('asr.funasrCapabilityLabel')" :help="t('asr.funasrCapabilityNote')">
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
    </SettingRow>
    <NAlert
      v-if="contextUnsupportedWarning"
      type="warning"
      :title="t('asr.funasrContextUnsupportedTitle')"
    >
      {{ t('asr.funasrContextUnsupportedBody') }}
    </NAlert>
    <SettingRow :label="t('asr.languageHint')" :help="t('asr.funasrLanguageNote')">
      <NInput
        v-model:value="funasrLanguage"
        :placeholder="t('asr.leaveEmptyToAuto')"
      />
    </SettingRow>
  </SettingsSection>
</template>

<style scoped>
@import '../../styles/asr-settings.css';
</style>
