<script setup lang="ts">
import { computed } from 'vue'
import { useI18n } from 'vue-i18n'
import type { UiLanguage } from '../i18n'

defineProps<{
  modelValue: UiLanguage
}>()

const emit = defineEmits<{
  'update:modelValue': [value: UiLanguage]
}>()

const { t } = useI18n()

const options = computed<Array<{ label: string; value: UiLanguage }>>(() => [
  { label: t('appearance.languageAuto'), value: 'system' },
  { label: '中文', value: 'zh-CN' },
  { label: 'EN', value: 'en-US' }
])
</script>

<template>
  <div class="language-switcher" role="radiogroup" :aria-label="t('appearance.language')">
    <button
      v-for="option in options"
      :key="option.value"
      type="button"
      role="radio"
      class="switch-option"
      :class="{ active: modelValue === option.value }"
      :aria-checked="modelValue === option.value"
      @click="emit('update:modelValue', option.value)"
    >
      {{ option.label }}
    </button>
  </div>
</template>

<style scoped>
.language-switcher {
  display: inline-flex;
  gap: 2px;
}

.switch-option {
  height: 24px;
  padding: 0 8px;
  border-radius: 5px;
  color: var(--color-text-tertiary);
  font-size: var(--font-sm);
  transition: color var(--transition-fast), background-color var(--transition-fast);
}

.switch-option:hover {
  color: var(--color-text-primary);
}

.switch-option.active {
  color: var(--color-text-primary);
  background: var(--sidebar-item-active);
}
</style>
