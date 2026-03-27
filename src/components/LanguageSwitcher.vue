<script setup lang="ts">
import { computed } from 'vue'
import { useI18n } from 'vue-i18n'
import type { ResolvedLocale, UiLanguage } from '../i18n'

const props = withDefaults(defineProps<{
  modelValue: UiLanguage
  resolvedLocale: ResolvedLocale
  showResolvedChip?: boolean
}>(), {
  showResolvedChip: true
})

const emit = defineEmits<{
  'update:modelValue': [value: UiLanguage]
}>()

const { t } = useI18n()

const options: Array<{ label: string; value: UiLanguage }> = [
  { label: 'Auto', value: 'system' },
  { label: '中文', value: 'zh-CN' },
  { label: 'EN', value: 'en-US' }
]

const selectedIndex = computed(() => {
  const index = options.findIndex((option) => option.value === props.modelValue)
  return index < 0 ? 0 : index
})

const resolvedLabel = computed(() => {
  return props.resolvedLocale === 'zh-CN'
    ? t('common.languages.zhCN')
    : t('common.languages.enUS')
})

function updateLanguage(value: UiLanguage) {
  emit('update:modelValue', value)
}
</script>

<template>
  <div class="language-switcher">
    <div class="switch-track" role="tablist" aria-label="Interface language">
      <div class="switch-thumb" :style="{ transform: `translateX(${selectedIndex * 100}%)` }" />
      <button
        v-for="option in options"
        :key="option.value"
        type="button"
        class="switch-option"
        :class="{ active: modelValue === option.value }"
        @click="updateLanguage(option.value)"
      >
        {{ option.label }}
      </button>
    </div>
    <span v-if="showResolvedChip && modelValue === 'system'" class="resolved-chip">
      {{ t('appHeader.systemResolved', { language: resolvedLabel }) }}
    </span>
  </div>
</template>

<style scoped>
.language-switcher {
  display: inline-flex;
  align-items: center;
  gap: 10px;
}

.switch-track {
  position: relative;
  display: inline-grid;
  grid-template-columns: repeat(3, 1fr);
  min-width: 156px;
  padding: 3px;
  border-radius: 999px;
  border: 1px solid var(--color-border);
  background: rgba(255, 255, 255, 0.04);
  backdrop-filter: blur(10px);
}

.switch-thumb {
  position: absolute;
  top: 3px;
  left: 3px;
  width: calc((100% - 6px) / 3);
  height: calc(100% - 6px);
  border-radius: 999px;
  background: rgba(255, 255, 255, 0.08);
  box-shadow: inset 0 1px 0 rgba(255, 255, 255, 0.05);
  transition: transform var(--transition-normal);
}

.switch-option {
  position: relative;
  z-index: 1;
  min-width: 50px;
  height: 28px;
  padding: 0 12px;
  border-radius: 999px;
  color: var(--color-text-secondary);
  font-size: var(--font-sm);
  font-weight: 600;
  transition: color var(--transition-fast);
}

.switch-option.active {
  color: var(--color-text-primary);
}

.resolved-chip {
  display: inline-flex;
  align-items: center;
  padding: 5px 10px;
  border-radius: 999px;
  border: 1px solid var(--color-border);
  color: var(--color-text-tertiary);
  font-size: var(--font-xs);
  white-space: nowrap;
}

@media (max-width: 960px) {
  .language-switcher {
    gap: 8px;
  }

  .resolved-chip {
    display: none;
  }
}
</style>
