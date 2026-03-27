<script setup lang="ts">
import { computed } from 'vue'
import { NInput, NButton } from 'naive-ui'
import { useI18n } from 'vue-i18n'
import { useSettingsStore } from '../stores/settings'

const settingsStore = useSettingsStore()
const { t } = useI18n()

const dictionaryText = computed({
  get: () => settingsStore.settings.dictionaryText,
  set: (value: string) => settingsStore.updateSetting('dictionaryText', value)
})

const wordCount = computed(() => {
  const text = dictionaryText.value.trim()
  if (!text) return 0
  return text.split('\n').filter(line => line.trim()).length
})

async function handleBlur() {
  scheduleSync('blur', 0)
}

let syncTimeout: number | null = null

function scheduleSync(_reason: string, delay = 800) {
  if (syncTimeout !== null) {
    clearTimeout(syncTimeout)
  }
  syncTimeout = window.setTimeout(async () => {
    await settingsStore.forceSaveSettings()
    // Online hotword sync disabled — inline hotwords are sent directly to ASR.
    syncTimeout = null
  }, delay)
}

function trimBlankLines() {
  const lines = dictionaryText.value
    .split('\n')
    .map(line => line.trim())
    .filter(line => line.length > 0)
  dictionaryText.value = lines.join('\n')
  scheduleSync('trim', 0)
}

</script>

<template>
  <div class="page dictionary-page">
    <div class="page-header">
      <h1 class="page-title">{{ t('dictionary.title') }}</h1>
      <div class="page-subtitle">
        <span class="subtitle-item">
          <span class="subtitle-label">{{ t('dictionary.entries') }}</span>
          <span class="pill">{{ t('dictionary.entriesCount', { count: wordCount }) }}</span>
        </span>
        <span class="subtitle-item">
          <span class="subtitle-label">{{ t('dictionary.mode') }}</span>
          <span class="pill">{{ t('dictionary.onePerLineShort') }}</span>
        </span>
      </div>
    </div>

    <div class="surface-card">
      <div class="section-header">
        <div>
          <div class="section-title">{{ t('dictionary.onePerLine') }}</div>
          <div class="section-hint">{{ t('dictionary.sectionHint') }}</div>
        </div>
        <div class="actions">
          <NButton size="small" quaternary @click="trimBlankLines">{{ t('dictionary.trimBlankLines') }}</NButton>
        </div>
      </div>

      <NInput
        v-model:value="dictionaryText"
        type="textarea"
        :placeholder="t('dictionary.editorPlaceholder')"
        :rows="16"
        class="dictionary-editor"
        @blur="handleBlur"
      />
    </div>

    <div class="section-hint">
      {{ t('dictionary.asyncHint') }}
    </div>
  </div>
</template>

<style scoped>
.dictionary-page {
  max-width: 1000px;
}

.dictionary-editor :deep(.n-input__textarea-el) {
  font-family: ui-monospace, monospace;
  font-size: var(--font-md);
  line-height: 1.6;
  min-height: 360px;
}

.actions {
  display: flex;
  gap: var(--spacing-sm);
}
</style>
