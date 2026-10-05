<script setup lang="ts">
import { onErrorCaptured, ref, watch } from 'vue'
import { NButton } from 'naive-ui'
import { useI18n } from 'vue-i18n'

const props = defineProps<{ routeKey: string }>()
const { t } = useI18n()
const failure = ref<string | null>(null)
const attempt = ref(0)

onErrorCaptured((error) => {
  failure.value = String(error)
  // Keep propagation to app.config.errorHandler for diagnostic reporting.
})

function retry() {
  failure.value = null
  attempt.value++
}

watch(() => props.routeKey, () => {
  if (failure.value !== null) retry()
})
</script>

<template>
  <div v-if="failure !== null" class="page" role="alert">
    <h1 class="page-title">{{ t('pageError.title') }}</h1>
    <div class="surface-card page-error-card">
      <p>{{ t('pageError.description') }}</p>
      <pre class="page-error-detail">{{ failure }}</pre>
      <NButton type="primary" secondary @click="retry">
        {{ t('pageError.retry') }}
      </NButton>
    </div>
  </div>
  <template v-else :key="attempt">
    <slot />
  </template>
</template>

<style scoped>
.page-error-card {
  display: flex;
  flex-direction: column;
  align-items: flex-start;
  gap: var(--spacing-md);
  color: var(--color-text-secondary);
}

.page-error-detail {
  width: 100%;
  white-space: pre-wrap;
  overflow-wrap: anywhere;
  user-select: text;
  -webkit-user-select: text;
  color: var(--color-text-primary);
}
</style>
