<script setup lang="ts">
import { computed, onMounted } from 'vue'
import { NSelect } from 'naive-ui'
import { useI18n } from 'vue-i18n'
import { usePostEditStore } from '../stores/postEdit'

const store = usePostEditStore()
const { t } = useI18n()
const options = computed(() => ['off', 'metrics_only', 'records'].map(value => ({
  value, label: t(`postEdit.modes.${value}`)
})))
onMounted(() => { void store.load() })
</script>

<template>
  <div class="post-edit-mode">
    <NSelect
      :value="store.settings?.mode ?? null"
      :options="options"
      :loading="store.saving"
      :disabled="!store.settings || store.saving"
      :aria-label="t('postEdit.modeLabel')"
      :placeholder="t('postEdit.modeLabel')"
      @update:value="store.setMode"
    />
    <div v-if="store.error" class="mode-error" role="alert">
      {{ store.error }}
      <button type="button" @click="store.load">{{ t('postEdit.retry') }}</button>
    </div>
  </div>
</template>

<style scoped>
.post-edit-mode { width: 210px; flex-shrink: 0; }
.mode-error { margin-top: 6px; color: var(--color-error); font-size: var(--font-xs); overflow-wrap: anywhere; }
.mode-error button { color: inherit; background: none; border: 0; text-decoration: underline; cursor: pointer; }
</style>
