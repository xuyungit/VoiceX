<script setup lang="ts">
import { computed } from 'vue'
import { NSelect } from 'naive-ui'
import { useI18n } from 'vue-i18n'
import { buildModelOptions, findAsrModel, modelSelectionError, type ModelMode } from '../../utils/asrModels'

const props = defineProps<{ value: string; provider: string; mode: ModelMode }>()
const emit = defineEmits<{ 'update:value': [value: string] }>()
const { t } = useI18n()
const options = computed(() => buildModelOptions(props.provider, props.mode, props.value, t))
const invalid = computed(() => modelSelectionError(props.provider, props.value, props.mode))
const custom = computed(() => props.value && !findAsrModel(props.provider, props.value))
function updateValue(value: string) {
  emit('update:value', value.trim())
}
</script>

<template>
  <div class="model-select">
    <NSelect :value="value" :options="options" filterable tag size="small"
      :status="invalid ? 'error' : undefined"
      :placeholder="t('asr.modelSelectPlaceholder')"
      @update:value="updateValue" />
    <div v-if="invalid" class="model-warning" role="alert">{{ t('asr.modelModeMismatch') }}</div>
    <div v-else-if="custom" class="model-note">{{ t('asr.modelCustomNote') }}</div>
  </div>
</template>

<style scoped>
.model-select { min-width: 0; }
.model-note, .model-warning { margin-top: 6px; font-size: 12px; line-height: 1.5; }
.model-note { color: var(--text-secondary); }
.model-warning { color: #e88080; }
</style>
