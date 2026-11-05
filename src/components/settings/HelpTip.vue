<script setup lang="ts">
import { ref } from 'vue'
import { NPopover } from 'naive-ui'

defineProps<{ text: string }>()

// Hover previews the help; click or keyboard focus pins it open so it can be
// read without holding the pointer still.
const hovered = ref(false)
const pinned = ref(false)
</script>

<template>
  <NPopover
    :show="hovered || pinned"
    placement="top-start"
    :show-arrow="false"
    raw
    @clickoutside="pinned = false"
  >
    <template #trigger>
      <button
        type="button"
        class="help-tip"
        :class="{ active: hovered || pinned }"
        :aria-label="text"
        @mouseenter="hovered = true"
        @mouseleave="hovered = false"
        @click.stop="pinned = !pinned"
        @focus="pinned = true"
        @blur="pinned = false"
        @keydown.esc="pinned = false"
      >?</button>
    </template>
    <div class="help-tip-body">{{ text }}</div>
  </NPopover>
</template>

<style scoped>
.help-tip {
  display: inline-flex;
  align-items: center;
  justify-content: center;
  width: 15px;
  height: 15px;
  flex: 0 0 auto;
  border-radius: 50%;
  border: 1px solid var(--color-text-disabled);
  color: var(--color-text-tertiary);
  font-size: 10px;
  font-weight: 600;
  line-height: 1;
  transition: color var(--transition-fast), border-color var(--transition-fast), background var(--transition-fast);
}

.help-tip:hover,
.help-tip.active {
  color: var(--color-text-primary);
  border-color: var(--color-text-tertiary);
  background: var(--color-bg-hover);
}

.help-tip:focus-visible {
  outline-offset: 1px;
}

.help-tip-body {
  max-width: 300px;
  padding: 10px 12px;
  border-radius: var(--radius-md);
  border: 1px solid var(--color-border);
  background: var(--color-bg-tertiary);
  box-shadow: var(--shadow-lg);
  color: var(--color-text-secondary);
  font-size: var(--font-sm);
  line-height: 1.55;
  user-select: text;
}
</style>
