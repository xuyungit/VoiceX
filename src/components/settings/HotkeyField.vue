<script setup lang="ts">
import { useI18n } from 'vue-i18n'

// Shows a recorded hotkey with an inline clear button; recording itself is
// started by the caller's own button so each page keeps its own flow.
defineProps<{
  value: string
  recording?: boolean
  empty?: boolean
  clearable?: boolean
  disabled?: boolean
}>()
const emit = defineEmits<{ clear: [] }>()
const { t } = useI18n()
</script>

<template>
  <div class="hotkey-field" :class="{ recording, empty: empty || recording, disabled }">
    <span class="hotkey-value" :title="value">{{ value }}</span>
    <button
      v-if="clearable && !recording"
      type="button"
      class="hotkey-clear"
      :title="t('input.clear')"
      :aria-label="t('input.clear')"
      :disabled="disabled"
      @click="emit('clear')"
    >
      <svg viewBox="0 0 16 16" width="10" height="10" aria-hidden="true">
        <path d="M4 4l8 8M12 4l-8 8" stroke="currentColor" stroke-width="1.6" stroke-linecap="round" />
      </svg>
    </button>
  </div>
</template>

<style scoped>
.hotkey-field {
  flex: 1;
  min-width: 0;
  height: 28px;
  display: flex;
  align-items: center;
  gap: 6px;
  padding: 0 6px 0 10px;
  border-radius: var(--radius-sm);
  border: 1px solid var(--color-field-border);
  background: var(--color-field-bg);
  transition: border-color var(--transition-fast), box-shadow var(--transition-fast);
}

.hotkey-field.recording {
  border-color: var(--color-accent);
  box-shadow: 0 0 0 2px var(--color-accent-light);
}

.hotkey-field.disabled {
  opacity: 0.55;
}

.hotkey-value {
  flex: 1;
  min-width: 0;
  overflow: hidden;
  text-overflow: ellipsis;
  white-space: nowrap;
  font-size: var(--font-md);
  color: var(--color-text-primary);
}

.hotkey-field.empty .hotkey-value {
  color: var(--color-text-tertiary);
}

.hotkey-clear {
  display: inline-flex;
  align-items: center;
  justify-content: center;
  width: 18px;
  height: 18px;
  flex: 0 0 auto;
  border-radius: 50%;
  color: var(--color-text-tertiary);
  transition: color var(--transition-fast), background var(--transition-fast);
}

.hotkey-clear:hover:not(:disabled) {
  color: var(--color-text-primary);
  background: var(--color-bg-hover);
}
</style>
