<script setup lang="ts">
import { computed } from 'vue'
import { NInput } from 'naive-ui'
import { useI18n } from 'vue-i18n'
import { useSettingsStore } from '../../stores/settings'

const settingsStore = useSettingsStore()
const { t } = useI18n()

const cohereApiKey = computed({
  get: () => settingsStore.settings.cohereApiKey,
  set: (v: string) => settingsStore.updateSetting('cohereApiKey', v)
})

const cohereModel = computed({
  get: () => settingsStore.settings.cohereModel,
  set: (v: string) => settingsStore.updateSetting('cohereModel', v)
})

const cohereLanguage = computed({
  get: () => settingsStore.settings.cohereLanguage,
  set: (v: string) => settingsStore.updateSetting('cohereLanguage', v)
})
</script>

<template>
  <div class="surface-card asr-card">
    <div class="card-header">
      <div class="card-title">{{ t('asr.cohereConfiguration') }}</div>
      <div class="card-sub">{{ t('asr.cohereConfigurationSub') }}</div>
    </div>
    <div class="field-list">
      <div class="field-row">
        <div class="field-text">
          <div class="field-label">{{ t('asr.apiCredentials') }}</div>
          <div class="field-note">{{ t('asr.cohereApiKeyNote') }}</div>
        </div>
        <NInput
          v-model:value="cohereApiKey"
          type="password"
          show-password-on="click"
          placeholder="Enter Cohere API key"
          class="field-control"
        />
      </div>
      <div class="field-row">
        <div class="field-text">
          <div class="field-label">{{ t('asr.model') }}</div>
          <div class="field-note">{{ t('asr.cohereModelNote') }}</div>
        </div>
        <NInput
          v-model:value="cohereModel"
          placeholder="cohere-transcribe-03-2026"
          class="field-control"
        />
      </div>
      <div class="field-row">
        <div class="field-text">
          <div class="field-label">{{ t('asr.languageHint') }}</div>
          <div class="field-note">{{ t('asr.cohereLanguageNote') }}</div>
        </div>
        <NInput v-model:value="cohereLanguage" placeholder="zh" class="field-control" />
      </div>
    </div>
  </div>
</template>

<style scoped>
@import '../../styles/asr-settings.css';
</style>
