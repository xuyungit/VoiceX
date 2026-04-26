<script setup lang="ts">
import { computed } from 'vue'
import { NInput, NSelect } from 'naive-ui'
import { useI18n } from 'vue-i18n'
import { useSettingsStore } from '../../stores/settings'

const settingsStore = useSettingsStore()
const { t } = useI18n()

const stepaudioApiKey = computed({
  get: () => settingsStore.settings.stepaudioApiKey,
  set: (v: string) => settingsStore.updateSetting('stepaudioApiKey', v)
})

const stepaudioModel = computed({
  get: () => settingsStore.settings.stepaudioModel,
  set: (v: string) => settingsStore.updateSetting('stepaudioModel', v)
})

const stepaudioBaseUrl = computed({
  get: () => settingsStore.settings.stepaudioBaseUrl,
  set: (v: string) => settingsStore.updateSetting('stepaudioBaseUrl', v)
})

const stepaudioLanguage = computed({
  get: () => settingsStore.settings.stepaudioLanguage,
  set: (v: 'auto' | 'zh' | 'en' | '') => settingsStore.updateSetting('stepaudioLanguage', v)
})

const stepaudioLanguageOptions = computed(() => [
  { label: t('asr.stepaudioLanguageAuto'), value: 'auto' },
  { label: t('asr.stepaudioLanguageZh'), value: 'zh' },
  { label: t('asr.stepaudioLanguageEn'), value: 'en' },
  { label: t('asr.stepaudioLanguageBlank'), value: '' }
])
</script>

<template>
  <div class="surface-card asr-card">
    <div class="card-header">
      <div class="card-title">{{ t('asr.stepaudioConfiguration') }}</div>
      <div class="card-sub">{{ t('asr.stepaudioConfigurationSub') }}</div>
    </div>
    <div class="field-list">
      <div class="field-row">
        <div class="field-text">
          <div class="field-label">{{ t('asr.apiCredentials') }}</div>
          <div class="field-note">{{ t('asr.stepaudioApiKeyNote') }}</div>
        </div>
        <NInput
          v-model:value="stepaudioApiKey"
          type="password"
          show-password-on="click"
          placeholder="StepFun API key"
          class="field-control"
        />
      </div>
      <div class="field-row">
        <div class="field-text">
          <div class="field-label">{{ t('asr.model') }}</div>
          <div class="field-note">{{ t('asr.stepaudioModelNote') }}</div>
        </div>
        <NInput
          v-model:value="stepaudioModel"
          placeholder="stepaudio-2.5-asr"
          class="field-control"
        />
      </div>
      <div class="field-row">
        <div class="field-text">
          <div class="field-label">{{ t('asr.baseUrl') }}</div>
          <div class="field-note">{{ t('asr.stepaudioBaseUrlNote') }}</div>
        </div>
        <NInput
          v-model:value="stepaudioBaseUrl"
          placeholder="https://api.stepfun.com/v1"
          class="field-control"
        />
      </div>
      <div class="field-row">
        <div class="field-text">
          <div class="field-label">{{ t('asr.languageHint') }}</div>
          <div class="field-note">{{ t('asr.stepaudioLanguageNote') }}</div>
        </div>
        <NSelect
          v-model:value="stepaudioLanguage"
          :options="stepaudioLanguageOptions"
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
