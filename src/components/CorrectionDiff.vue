<script setup lang="ts">
import { computed } from 'vue'
import { prefersInline, type TextDiff } from '../utils/textDiff'
import DiffText from './DiffText.vue'

// How a text was corrected: one paragraph with the changes marked when the
// edit is small, or both versions stacked with their changes marked when it
// rewrote too much to read inline.
const props = defineProps<{
  diff: TextDiff
  beforeLabel: string
  afterLabel: string
  /** Characters of unchanged context to keep around each change; all when unset. */
  context?: number
}>()

const inline = computed(() => prefersInline(props.diff))
</script>

<template>
  <p v-if="inline" class="correction-text">
    <DiffText :diff="diff" view="inline" :context="context" />
  </p>
  <div v-else class="correction-stacked">
    <div class="side">
      <div class="side-label">{{ beforeLabel }}</div>
      <p class="correction-text before"><DiffText :diff="diff" view="before" :context="context" /></p>
    </div>
    <div class="side">
      <div class="side-label">{{ afterLabel }}</div>
      <p class="correction-text"><DiffText :diff="diff" view="after" :context="context" /></p>
    </div>
  </div>
</template>

<style scoped>
.correction-text { margin: 0; font-size: 14px; line-height: 1.85; }
.correction-text.before { color: var(--color-text-secondary); }
.correction-stacked { display: flex; flex-direction: column; gap: 10px; }
.side-label { margin-bottom: 2px; font-size: var(--font-xs); color: var(--color-text-tertiary); }
</style>
