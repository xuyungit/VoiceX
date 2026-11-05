<script setup lang="ts">
import { Comment, Fragment, Text, type VNode } from 'vue'
import HelpTip from './HelpTip.vue'

// One setting per row: label (with optional help behind a "?") on the left,
// control on the right. The status slot is for live state and warnings only;
// explanations belong in `help` so the page stays scannable. `stacked` puts a
// wide control (text area, prompt editor) on its own line under the label.
defineProps<{
  label: string
  help?: string
  disabled?: boolean
  stacked?: boolean
}>()

// A status slot whose lines are all v-if'd away renders only comments; it
// should not leave an empty line under the label.
function hasContent(nodes: VNode[] | undefined): boolean {
  return (nodes ?? []).some((node) => {
    if (node.type === Comment) return false
    if (node.type === Text) return String(node.children ?? '').trim() !== ''
    if (node.type === Fragment) return hasContent(node.children as VNode[])
    return true
  })
}
</script>

<template>
  <div class="setting-row" :class="{ disabled, stacked }">
    <div class="setting-text">
      <div class="setting-label">
        <span>{{ label }}</span>
        <HelpTip v-if="help" :text="help" />
      </div>
      <div v-if="hasContent($slots.status?.())" class="setting-status">
        <slot name="status" />
      </div>
    </div>
    <div v-if="$slots.default" class="setting-control">
      <slot />
    </div>
  </div>
</template>

<style scoped>
.setting-row {
  display: flex;
  align-items: center;
  justify-content: space-between;
  gap: var(--spacing-xl);
  min-height: 52px;
  padding: 10px 0;
}

.setting-row + .setting-row {
  border-top: 1px solid var(--color-divider);
}

.setting-row.stacked {
  flex-direction: column;
  align-items: stretch;
  gap: var(--spacing-sm);
}

.setting-text {
  display: flex;
  flex-direction: column;
  gap: 3px;
  flex: 1;
  min-width: 0;
}

.setting-label {
  display: flex;
  align-items: center;
  gap: 6px;
  font-size: var(--font-md);
  font-weight: 500;
  color: var(--color-text-primary);
}

.setting-row.disabled .setting-label {
  color: var(--color-text-tertiary);
}

.setting-status {
  display: flex;
  flex-direction: column;
  gap: 2px;
  font-size: var(--font-xs);
  line-height: 1.5;
  color: var(--color-text-tertiary);
  max-width: 480px;
  user-select: text;
}

.setting-status :slotted(.is-warning) {
  color: color-mix(in srgb, var(--color-warning) 72%, var(--color-text-secondary));
}

.setting-status :slotted(.is-error) {
  color: color-mix(in srgb, var(--color-error) 72%, var(--color-text-secondary));
}

.setting-control {
  display: flex;
  align-items: center;
  justify-content: flex-end;
  gap: var(--spacing-sm);
  width: 320px;
  max-width: 50%;
  flex: 0 0 auto;
}

.setting-row.stacked .setting-control {
  width: 100%;
  max-width: none;
  justify-content: flex-start;
}

</style>
