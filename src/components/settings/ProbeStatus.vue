<script setup lang="ts">
import { NPopover } from 'naive-ui'

// Result of a provider test, shown next to the button that ran it. The label
// carries the verdict; details (input/output) open from it on click.
defineProps<{
  tone: 'ok' | 'warn' | 'error'
  label: string
}>()
</script>

<template>
  <NPopover trigger="click" placement="bottom-end" :disabled="!$slots.default" style="max-width: 380px">
    <template #trigger>
      <button type="button" class="probe-status" :class="[tone, { interactive: !!$slots.default }]">
        <span class="dot" aria-hidden="true" />
        <span>{{ label }}</span>
        <svg v-if="$slots.default" class="chevron" viewBox="0 0 24 24" fill="currentColor" aria-hidden="true">
          <path d="M7.41 8.59 12 13.17l4.59-4.58L18 10l-6 6-6-6z" />
        </svg>
      </button>
    </template>
    <div class="probe-details">
      <slot />
    </div>
  </NPopover>
</template>

<style scoped>
.probe-status {
  display: inline-flex;
  align-items: center;
  gap: 6px;
  height: 28px;
  padding: 0 8px;
  border-radius: 6px;
  font-size: var(--font-sm);
  color: var(--color-text-secondary);
  font-variant-numeric: tabular-nums;
  cursor: default;
}

.probe-status.interactive {
  cursor: pointer;
}

.probe-status.interactive:hover {
  background: var(--color-bg-hover);
  color: var(--color-text-primary);
}

.dot {
  width: 7px;
  height: 7px;
  border-radius: 50%;
  flex: none;
}

.ok .dot { background: var(--color-success); }
.warn .dot { background: var(--color-warning); }
.error .dot { background: var(--color-error); }
.error { color: var(--color-error); }

.chevron {
  width: 14px;
  height: 14px;
  opacity: 0.7;
}

.probe-details {
  display: flex;
  flex-direction: column;
  gap: 4px;
  font-size: var(--font-sm);
  line-height: 1.6;
  user-select: text;
}

.probe-details :slotted(.probe-detail-label) {
  color: var(--color-text-tertiary);
}

.probe-details :slotted(.probe-detail-label:not(:first-child)) {
  margin-top: 6px;
}

.probe-details :slotted(.probe-detail-text) {
  color: var(--color-text-primary);
  overflow-wrap: anywhere;
}

.probe-details :slotted(del) {
  color: var(--color-error);
  text-decoration: line-through;
}

.probe-details :slotted(ins) {
  text-decoration: none;
  border-radius: 3px;
  background: color-mix(in srgb, var(--color-success) 20%, transparent);
}
</style>
