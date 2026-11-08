<script setup lang="ts">
import { computed } from 'vue'
import { NButton } from 'naive-ui'
import { useI18n } from 'vue-i18n'
import { searchPreview } from '../utils/historySearch'

const props = defineProps<{ text: string; query: string }>()
const emit = defineEmits<{ expand: [] }>()
const { t } = useI18n()
const preview = computed(() => searchPreview(props.text, props.query))
</script>

<template>
  <span><template v-for="(part, index) in preview.parts" :key="index"><mark v-if="part.match">{{ part.text }}</mark><template v-else>{{ part.text }}</template></template><NButton v-if="preview.truncated" text size="tiny" class="expand-text" @click="emit('expand')">{{ t('history.showFullText') }}</NButton></span>
</template>

<style scoped>
mark {
  background: var(--color-search-highlight);
  color: inherit;
  border-radius: 2px;
  box-decoration-break: clone;
  -webkit-box-decoration-break: clone;
}
.expand-text { margin-left: 8px; color: var(--color-accent); vertical-align: baseline; }
</style>
