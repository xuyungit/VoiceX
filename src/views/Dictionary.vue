<script setup lang="ts">
import { computed, ref, nextTick } from 'vue'
import { useI18n } from 'vue-i18n'
import { useSettingsStore } from '../stores/settings'

const settingsStore = useSettingsStore()
const { t } = useI18n()

const inputValue = ref('')
const inputRef = ref<HTMLInputElement | null>(null)

const entries = computed(() => {
  const text = settingsStore.settings.dictionaryText.trim()
  if (!text) return [] as string[]
  return text.split('\n').filter(line => line.trim()).map(line => line.trim())
})

function addEntry() {
  const value = inputValue.value.trim()
  if (!value) return
  if (entries.value.includes(value)) {
    inputValue.value = ''
    return
  }
  const newEntries = [...entries.value, value]
  settingsStore.updateSetting('dictionaryText', newEntries.join('\n'))
  inputValue.value = ''
  settingsStore.forceSaveSettings()
  nextTick(() => inputRef.value?.focus())
}

function removeEntry(index: number) {
  const newEntries = entries.value.filter((_, i) => i !== index)
  settingsStore.updateSetting('dictionaryText', newEntries.join('\n'))
  settingsStore.forceSaveSettings()
}

function handleKeydown(e: KeyboardEvent) {
  if (e.key === 'Enter') {
    e.preventDefault()
    addEntry()
  }
}
</script>

<template>
  <div class="page dictionary-page">
    <div class="page-header">
      <h1 class="page-title">{{ t('dictionary.title') }}</h1>
      <div class="page-subtitle">
        <span class="subtitle-item">
          <span class="subtitle-label">{{ t('dictionary.entries') }}</span>
          <span class="pill">{{ t('dictionary.entriesCount', { count: entries.length }) }}</span>
        </span>
      </div>
    </div>

    <div class="surface-card">
      <div class="section-header">
        <div>
          <div class="section-title">{{ t('dictionary.tagTitle') }}</div>
          <div class="section-hint">{{ t('dictionary.sectionHint') }}</div>
        </div>
      </div>

      <div class="tags-container">
        <span v-for="(entry, index) in entries" :key="entry" class="tag-chip">
          <span class="tag-label">{{ entry }}</span>
          <button class="tag-remove" @click="removeEntry(index)" :aria-label="t('dictionary.removeEntry')">×</button>
        </span>
        <input
          ref="inputRef"
          v-model="inputValue"
          class="tag-input"
          :placeholder="t('dictionary.addPlaceholder')"
          @keydown="handleKeydown"
        />
      </div>

      <div class="input-hint">{{ t('dictionary.inputHint') }}</div>
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

.tags-container {
  display: flex;
  flex-wrap: wrap;
  gap: var(--spacing-sm);
  align-items: center;
  padding: var(--spacing-md);
  background: var(--color-bg-primary);
  border: 1px solid var(--color-border);
  border-radius: var(--radius-md);
  min-height: 48px;
}

.tag-chip {
  display: inline-flex;
  align-items: center;
  gap: var(--spacing-xs);
  padding: 4px 8px 4px 12px;
  background: var(--color-bg-tertiary);
  border: 1px solid var(--color-border);
  border-radius: var(--radius-lg);
  font-size: var(--font-md);
  color: var(--color-text-primary);
  line-height: 1.4;
  user-select: none;
}

.tag-label {
  white-space: nowrap;
}

.tag-remove {
  display: inline-flex;
  align-items: center;
  justify-content: center;
  width: 18px;
  height: 18px;
  border: none;
  background: transparent;
  color: var(--color-text-tertiary);
  font-size: 15px;
  line-height: 1;
  cursor: pointer;
  border-radius: 50%;
  padding: 0;
  transition: all var(--transition-fast);
}

.tag-remove:hover {
  background: var(--color-bg-hover);
  color: var(--color-text-primary);
}

.tag-input {
  flex: 1;
  min-width: 140px;
  border: none;
  outline: none;
  background: transparent;
  color: var(--color-text-primary);
  font-size: var(--font-md);
  font-family: inherit;
  padding: 4px 0;
}

.tag-input::placeholder {
  color: var(--color-text-disabled);
}

.input-hint {
  margin-top: var(--spacing-sm);
  font-size: var(--font-sm);
  color: var(--color-text-tertiary);
}
</style>
