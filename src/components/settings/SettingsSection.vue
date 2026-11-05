<script setup lang="ts">
import HelpTip from './HelpTip.vue'

defineProps<{
  title: string
  help?: string
}>()
</script>

<template>
  <section class="settings-section">
    <header class="section-head">
      <h2 class="section-title section-name">
        <span>{{ title }}</span>
        <HelpTip v-if="help" :text="help" />
      </h2>
      <div v-if="$slots.extra" class="section-extra">
        <slot name="extra" />
      </div>
    </header>
    <div class="surface-card section-body">
      <slot />
    </div>
  </section>
</template>

<style scoped>
.settings-section {
  container-type: inline-size;
}

.section-head {
  display: flex;
  align-items: center;
  justify-content: space-between;
  flex-wrap: wrap;
  gap: var(--spacing-sm) var(--spacing-lg);
  min-height: 28px;
  padding: 0 2px 6px;
}

.section-name {
  display: flex;
  align-items: center;
  gap: 6px;
}

.section-extra {
  display: flex;
  align-items: center;
  gap: var(--spacing-sm);
}

.section-body {
  padding-top: 0;
  padding-bottom: 0;
}
</style>
