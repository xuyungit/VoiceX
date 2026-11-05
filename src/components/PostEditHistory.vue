<script setup lang="ts">
import { computed, onBeforeUnmount, onMounted, ref } from 'vue'
import { invoke } from '@tauri-apps/api/core'
import { listen, type UnlistenFn } from '@tauri-apps/api/event'
import { NButton, NEmpty, NSpin, useDialog, useMessage } from 'naive-ui'
import { useI18n } from 'vue-i18n'
import { useHistoryStore } from '../stores/history'
import type { PostEditRecord, PostEditRecordPage } from '../types/postEdit'
import InlineTextEditor from './InlineTextEditor.vue'
import LoadMoreSentinel from './LoadMoreSentinel.vue'

const historyStore = useHistoryStore()
const { t, locale } = useI18n()
const dialog = useDialog()
const message = useMessage()
const records = ref<PostEditRecord[]>([])
const loading = ref(false)
const error = ref<string | null>(null)
const saveError = ref<string | null>(null)
const hasMore = ref(false)
const expanded = ref(new Set<string>())
const busy = ref(false)
const editingId = ref<string | null>(null)
const savingEdit = ref(false)
const pageSize = 20
let disposed = false
let request = 0
let unlisten: UnlistenFn | undefined

const groups = computed(() => {
  const result: { date: string; records: PostEditRecord[] }[] = []
  for (const record of records.value) {
    const date = new Date(record.timestamp).toLocaleDateString(locale.value, { year: 'numeric', month: 'long', day: 'numeric' })
    const group = result[result.length - 1]
    if (group?.date === date) group.records.push(record)
    else result.push({ date, records: [record] })
  }
  return result
})

function time(at: string) {
  return new Date(at).toLocaleTimeString(locale.value, { hour: '2-digit', minute: '2-digit' })
}

async function load(more = false) {
  const id = ++request
  loading.value = true
  try {
    // Preserve loaded depth when a live update arrives; offset pages are read
    // from scratch after mutations so inserted/deleted rows cannot cause gaps.
    const count = more ? pageSize : Math.max(pageSize, records.value.length)
    const next: PostEditRecord[] = []
    let lastPageFull = false
    while (next.length < count) {
      const limit = Math.min(100, count - next.length)
      const result = await invoke<PostEditRecordPage>('get_post_edit_records', {
        limit, offset: (more ? records.value.length : 0) + next.length
      })
      if (disposed || id !== request) return
      const page = result.records
      saveError.value = result.saveError
      next.push(...page)
      lastPageFull = page.length === limit
      if (!lastPageFull) break
    }
    if (disposed || id !== request) return
    records.value = more ? [...records.value, ...next] : next
    hasMore.value = lastPageFull
    error.value = null
  } catch (e) {
    if (!disposed && id === request) error.value = String(e)
  } finally {
    if (!disposed && id === request) loading.value = false
  }
}

async function copy(text: string) {
  try {
    await navigator.clipboard.writeText(text)
    message.success(t('postEdit.copied'))
  } catch (e) { message.error(String(e)) }
}

function toggle(id: string) {
  const next = new Set(expanded.value)
  if (next.has(id)) next.delete(id)
  else next.add(id)
  expanded.value = next
}

function isLong(record: PostEditRecord) {
  return record.originalText.length > 180 || record.editedText.length > 180
    || record.originalText.split('\n').length > 4 || record.editedText.split('\n').length > 4
}

async function saveEdit(record: PostEditRecord, text: string) {
  savingEdit.value = true
  try {
    await historyStore.setFinalText(record.recordId, text)
    editingId.value = null
  } catch (e) {
    message.error(t('postEdit.saveFailed', { reason: String(e) }))
  } finally {
    savingEdit.value = false
  }
}

function remove(record: PostEditRecord) {
  dialog.warning({
    title: t('postEdit.deleteTitle'),
    content: t('postEdit.deleteHint'),
    positiveText: t('common.delete'), negativeText: t('postEdit.cancel'),
    onPositiveClick: async () => {
      busy.value = true
      try {
        await invoke('delete_post_edit_record', { id: record.id, recordId: record.recordId })
        await load()
      } catch (e) { message.error(String(e)) }
      finally { busy.value = false }
    }
  })
}

onMounted(async () => {
  // Subscribe first so a record completed while loading is not missed.
  try {
    const stop = await listen('post-edit:updated', () => { void load() })
    if (disposed) { stop(); return }
    unlisten = stop
  } catch (e) { message.error(String(e)) }
  if (!disposed) void load()
})
onBeforeUnmount(() => { disposed = true; ++request; unlisten?.() })
</script>

<template>
  <section class="post-edit-history" :aria-label="t('postEdit.tab')">
    <div v-if="error" class="records-error" role="alert">
      <span>{{ t('postEdit.loadError') }} {{ error }}</span>
      <NButton size="small" @click="load()">{{ t('postEdit.retry') }}</NButton>
    </div>
    <div v-if="saveError" class="records-error" role="alert">{{ t('postEdit.saveError') }} {{ saveError }}</div>
    <div v-if="loading && !records.length" class="records-empty"><NSpin /></div>
    <div v-else-if="!records.length && !error" class="records-empty">
      <NEmpty :description="t('postEdit.empty')">
        <template #extra><p class="empty-hint">{{ t('postEdit.emptyHint') }}</p></template>
      </NEmpty>
    </div>

    <div v-for="group in groups" :key="group.date" class="record-group">
      <h3 class="record-date">{{ group.date }}</h3>
      <article v-for="record in group.records" :key="record.id" class="edit-record">
        <header class="record-meta">
          <div class="meta-main">
            <time :datetime="record.timestamp">{{ time(record.timestamp) }}</time>
            <span v-if="record.appName" class="app-name">{{ record.appName }}</span>
          </div>
          <NButton quaternary size="tiny" :disabled="busy" @click="remove(record)">{{ t('common.delete') }}</NButton>
        </header>
        <div class="text-pair" :class="{ expanded: expanded.has(record.id) || editingId === record.id, collapsible: isLong(record) }">
          <div class="text-column original">
            <div class="text-label"><span>{{ t('postEdit.original') }}</span><NButton text size="tiny" @click="copy(record.originalText)">{{ t('postEdit.copy') }}</NButton></div>
            <p class="record-text">{{ record.originalText }}</p>
          </div>
          <div class="text-column edited">
            <div class="text-label">
              <span>{{ t('postEdit.edited') }}</span>
              <span v-if="editingId !== record.id" class="label-actions">
                <NButton text size="tiny" @click="editingId = record.id">{{ t('postEdit.edit') }}</NButton>
                <NButton text size="tiny" @click="copy(record.editedText)">{{ t('postEdit.copy') }}</NButton>
              </span>
            </div>
            <InlineTextEditor
              v-if="editingId === record.id"
              :text="record.editedText"
              :original="record.originalText"
              :saving="savingEdit"
              @save="text => saveEdit(record, text)"
              @cancel="editingId = null"
            />
            <p v-else class="record-text">{{ record.editedText }}</p>
          </div>
        </div>
        <footer v-if="isLong(record) && editingId !== record.id" class="record-footer">
          <NButton text size="tiny" @click="toggle(record.id)">
            {{ t(expanded.has(record.id) ? 'postEdit.collapse' : 'postEdit.expand') }}
          </NButton>
        </footer>
      </article>
    </div>
    <LoadMoreSentinel v-if="hasMore && !error" :loading="loading" @load="load(true)" />
  </section>
</template>

<style scoped>
.post-edit-history { min-width: 0; }
.records-empty { display: flex; align-items: center; justify-content: center; min-height: 280px; padding: 32px; }
.empty-hint { max-width: 420px; margin: 0 auto; font-size: var(--font-sm); color: var(--color-text-tertiary); line-height: 1.8; }
.records-error { display: flex; gap: 16px; justify-content: space-between; align-items: center; padding: 16px 0; color: var(--color-error); overflow-wrap: anywhere; }
.record-group { margin-top: 20px; }
.record-group:first-of-type { margin-top: 4px; }
.record-date { margin: 0 0 6px; font-size: var(--font-xs); font-weight: 600; color: var(--color-text-tertiary); }
.edit-record { padding: 16px 0; border-bottom: 1px solid var(--color-border); }
.record-meta, .meta-main { display: flex; align-items: center; gap: 12px; }
.record-meta { justify-content: space-between; margin-bottom: 12px; }
.meta-main { min-width: 0; }
.record-meta time { font-size: var(--font-sm); font-variant-numeric: tabular-nums; font-weight: 600; }
.app-name { color: var(--color-text-tertiary); font-size: var(--font-xs); max-width: 260px; overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
.text-pair { display: grid; grid-template-columns: minmax(0, 1fr) minmax(0, 1fr); gap: 28px; }
.text-column { min-width: 0; }
.text-column.edited { padding-left: 22px; border-left: 2px solid var(--color-accent); }
.text-label { display: flex; align-items: center; justify-content: space-between; min-height: 20px; margin-bottom: 8px; font-size: var(--font-xs); color: var(--color-text-tertiary); }
.label-actions { display: inline-flex; gap: 12px; }
.record-text { font-size: 14px; line-height: 1.85; white-space: pre-wrap; overflow-wrap: anywhere; margin: 0; }
.original .record-text { color: var(--color-text-secondary); }
.text-pair.collapsible:not(.expanded) .record-text { display: -webkit-box; -webkit-line-clamp: 5; -webkit-box-orient: vertical; overflow: hidden; }
.record-footer { display: flex; justify-content: flex-end; margin-top: 10px; }
@media (max-width: 760px) {
  .text-pair { grid-template-columns: minmax(0, 1fr); gap: 18px; }
  .text-column.edited { padding-left: 12px; }
}
</style>
