<script setup lang="ts">
import HelpTip from './HelpTip.vue'

defineProps<{
  title: string
  help?: string
}>()
</script>

<template>
  <section class="surface-card settings-section">
    <header class="section-head">
      <h2 class="section-name">
        <span>{{ title }}</span>
        <HelpTip v-if="help" :text="help" />
      </h2>
      <div v-if="$slots.extra" class="section-extra">
        <slot name="extra" />
      </div>
    </header>
    <div class="section-body">
      <slot />
    </div>
  </section>
</template>

<style scoped>
.settings-section {
  padding: var(--spacing-lg) var(--spacing-xl) 6px;
  container-type: inline-size;
}

.section-head {
  display: flex;
  align-items: center;
  justify-content: space-between;
  flex-wrap: wrap;
  gap: var(--spacing-sm) var(--spacing-lg);
  padding-bottom: 6px;
}

.section-name {
  display: flex;
  align-items: center;
  gap: 6px;
  font-size: var(--font-xs);
  font-weight: 600;
  letter-spacing: 0.06em;
  text-transform: uppercase;
  color: var(--color-text-tertiary);
}

.section-extra {
  display: flex;
  align-items: center;
  gap: var(--spacing-sm);
}
</style>
