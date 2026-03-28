<script setup lang="ts">
import { computed } from 'vue'
import { NInput, NSelect } from 'naive-ui'
import { useI18n } from 'vue-i18n'
import { useSettingsStore } from '../../stores/settings'

const settingsStore = useSettingsStore()
const { t } = useI18n()

const geminiApiKey = computed({
  get: () => settingsStore.settings.geminiApiKey,
  set: (v: string) => settingsStore.updateSetting('geminiApiKey', v)
})

const geminiModel = computed({
  get: () => settingsStore.settings.geminiModel,
  set: (v: string) => settingsStore.updateSetting('geminiModel', v)
})

const geminiLanguage = computed({
  get: () => settingsStore.settings.geminiLanguage,
  set: (v: 'auto' | 'zh' | 'en' | 'zh-en') => settingsStore.updateSetting('geminiLanguage', v)
})

const geminiLanguageOptions = computed(() => [
  { label: t('asr.geminiLanguageAuto'), value: 'auto' },
  { label: t('asr.geminiLanguageZh'), value: 'zh' },
  { label: t('asr.geminiLanguageEn'), value: 'en' },
  { label: t('asr.geminiLanguageZhEn'), value: 'zh-en' },
])
</script>

<template>
  <div class="surface-card asr-card">
    <div class="card-header">
      <div class="card-title">{{ t('asr.geminiConfiguration') }}</div>
      <div class="card-sub">{{ t('asr.geminiConfigurationSub') }}</div>
    </div>
    <div class="field-list">
      <div class="field-row">
        <div class="field-text">
          <div class="field-label">{{ t('asr.apiCredentials') }}</div>
          <div class="field-note">{{ t('asr.geminiApiKeyNote') }}</div>
        </div>
        <NInput
          v-model:value="geminiApiKey"
          type="password"
          show-password-on="click"
          placeholder="AIza..."
          class="field-control"
        />
      </div>
      <div class="field-row">
        <div class="field-text">
          <div class="field-label">{{ t('asr.model') }}</div>
          <div class="field-note">{{ t('asr.geminiModelNote') }}</div>
        </div>
        <NInput
          v-model:value="geminiModel"
          placeholder="gemini-3.1-flash-lite-preview"
          class="field-control"
        />
      </div>
      <div class="field-row">
        <div class="field-text">
          <div class="field-label">{{ t('asr.languageHint') }}</div>
          <div class="field-note">{{ t('asr.geminiLanguageNote') }}</div>
        </div>
        <NSelect
          v-model:value="geminiLanguage"
          :options="geminiLanguageOptions"
          size="small"
          class="field-control"
        />
      </div>
    </div>
  </div>
</template>

<style scoped>
@import '../../styles/asr-settings.css';
</style>
