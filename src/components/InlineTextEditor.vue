<script setup lang="ts">
import { computed, nextTick, onMounted, ref } from 'vue'
import { NButton, NInput, type InputInst } from 'naive-ui'
import { useI18n } from 'vue-i18n'

// One editing surface for every history view: the dictation list and the
// edit records both save a dictation's final text through it.
const props = defineProps<{
  text: string
  original?: string | null
  saving?: boolean
}>()
const emit = defineEmits<{ save: [text: string]; cancel: [] }>()
defineSlots<{ preview?: (props: { draft: string }) => unknown }>()

const { t } = useI18n()
const draft = ref(props.text)
const input = ref<InputInst | null>(null)
const isMac = /Mac|iPhone|iPad/.test(navigator.userAgent)
const saveKey = isMac ? '⌘↩' : 'Ctrl+Enter'

const canSave = computed(() => !props.saving && draft.value.trim().length > 0 && draft.value !== props.text)
const canRestore = computed(() => props.original != null && draft.value !== props.original)

function save() {
  if (canSave.value) emit('save', draft.value)
}

function restore() {
  if (props.original == null) return
  draft.value = props.original
  input.value?.focus()
}

function onKeydown(event: KeyboardEvent) {
  if (event.isComposing) return
  if (event.key === 'Enter' && (event.metaKey || event.ctrlKey)) {
    event.preventDefault()
    save()
  } else if (event.key === 'Escape') {
    event.preventDefault()
    event.stopPropagation()
    emit('cancel')
  }
}

onMounted(async () => {
  await nextTick()
  input.value?.focus()
  const el = input.value?.textareaElRef
  if (el) el.setSelectionRange(el.value.length, el.value.length)
})
</script>

<template>
  <div class="inline-editor">
    <NInput
      ref="input"
      v-model:value="draft"
      type="textarea"
      :autosize="{ minRows: 2, maxRows: 14 }"
      :disabled="saving"
      @keydown="onKeydown"
    />
    <div class="editor-bar">
      <span class="editor-hint">{{ t('postEdit.editorHint', { key: saveKey }) }}</span>
      <div class="editor-actions">
        <NButton v-if="canRestore" text size="tiny" class="restore" :disabled="saving" @click="restore">
          {{ t('postEdit.restoreOriginal') }}
        </NButton>
        <NButton quaternary size="tiny" :disabled="saving" @click="emit('cancel')">{{ t('postEdit.cancel') }}</NButton>
        <NButton type="primary" size="tiny" :disabled="!canSave" :loading="saving" @click="save">{{ t('postEdit.save') }}</NButton>
      </div>
    </div>
    <!-- Live feedback on the draft, e.g. its diff against the original. -->
    <slot name="preview" :draft="draft" />
  </div>
</template>

<style scoped>
.inline-editor { display: flex; flex-direction: column; gap: 6px; min-width: 0; }
.inline-editor :deep(textarea) { line-height: 1.75; }
.editor-bar { display: flex; align-items: center; justify-content: space-between; gap: 12px; }
.editor-hint { font-size: 11px; color: var(--color-text-tertiary); white-space: nowrap; overflow: hidden; text-overflow: ellipsis; }
.editor-actions { display: flex; align-items: center; gap: 8px; flex-shrink: 0; }
.restore { margin-right: 4px; color: var(--color-text-secondary); }
</style>
