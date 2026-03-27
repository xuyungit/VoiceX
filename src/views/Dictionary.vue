<script setup lang="ts">
import { computed } from 'vue'
import { NInput, NButton } from 'naive-ui'
import { useSettingsStore } from '../stores/settings'

const settingsStore = useSettingsStore()

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
      <h1 class="page-title">Dictionary</h1>
      <div class="page-subtitle">
        <span class="subtitle-item">
          <span class="subtitle-label">词条</span>
          <span class="pill">{{ wordCount }} entries</span>
        </span>
        <span class="subtitle-item">
          <span class="subtitle-label">模式</span>
          <span class="pill">每行一个词</span>
        </span>
      </div>
    </div>

    <div class="surface-card">
      <div class="section-header">
        <div>
          <div class="section-title">One entry per line.</div>
          <div class="section-hint">热词用于 ASR 和 LLM 纠错</div>
        </div>
        <div class="actions">
          <NButton size="small" quaternary @click="trimBlankLines">Trim Blank Lines</NButton>
        </div>
      </div>

      <NInput
        v-model:value="dictionaryText"
        type="textarea"
        placeholder="在此添加热词，每行一个。例如：&#10;VoiceX&#10;语音识别&#10;人工智能"
        :rows="16"
        class="dictionary-editor"
        @blur="handleBlur"
      />
    </div>

    <div class="section-hint">
      Async 模式最多使用前 100 个词条；nostream 模式最多 5000 个。
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
