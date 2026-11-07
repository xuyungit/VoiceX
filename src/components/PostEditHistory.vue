<script setup lang="ts">
import { computed, onBeforeUnmount, onMounted, ref } from 'vue'
import { invoke } from '@tauri-apps/api/core'
import { listen, type UnlistenFn } from '@tauri-apps/api/event'
import { NButton, NDropdown, NEmpty, NSpin, useDialog, useMessage, type DropdownOption } from 'naive-ui'
import { useI18n } from 'vue-i18n'
import { useHistoryStore } from '../stores/history'
import type { PostEditRecord, PostEditRecordPage } from '../types/postEdit'
import { condense, diffText, type TextDiff } from '../utils/textDiff'
import CorrectionDiff from './CorrectionDiff.vue'
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
/** Characters of unchanged text kept around each change in a collapsed record. */
const COLLAPSED_CONTEXT = 30
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

// Diffs are computed once per loaded record, not on every render.
const diffs = computed(() => {
  const result = new Map<string, { diff: TextDiff; collapsible: boolean }>()
  for (const record of records.value) {
    const diff = diffText(record.originalText, record.editedText)
    const collapsible = isLong(record) && condense(diff, COLLAPSED_CONTEXT).some(part => part.kind === 'gap')
    result.set(record.id, { diff, collapsible })
  }
  return result
})

const moreOptions = computed<DropdownOption[]>(() => [
  { label: t('postEdit.copyOriginal'), key: 'copy-original' },
  { label: t('common.delete'), key: 'delete' }
])

function onMore(key: string | number, record: PostEditRecord) {
  if (key === 'copy-original') void copy(record.originalText)
  else if (key === 'delete') remove(record)
}

function isLong(record: PostEditRecord) {
  const long = (text: string) => text.length > 180 || text.split('\n').length > 4
  return long(record.originalText) || long(record.editedText)
}

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
          <div v-if="editingId !== record.id" class="record-actions">
            <NButton text size="tiny" @click="editingId = record.id">{{ t('postEdit.edit') }}</NButton>
            <NButton text size="tiny" @click="copy(record.editedText)">{{ t('postEdit.copy') }}</NButton>
            <NDropdown trigger="click" placement="bottom-end" :options="moreOptions" @select="key => onMore(key, record)">
              <NButton quaternary size="tiny" class="more-button" :disabled="busy" :aria-label="t('common.moreActions')">
                <template #icon>
                  <svg viewBox="0 0 24 24" fill="currentColor" width="16" height="16" aria-hidden="true">
                    <path d="M6 10a2 2 0 1 0 0 4 2 2 0 0 0 0-4zm6 0a2 2 0 1 0 0 4 2 2 0 0 0 0-4zm6 0a2 2 0 1 0 0 4 2 2 0 0 0 0-4z" />
                  </svg>
                </template>
              </NButton>
            </NDropdown>
          </div>
        </header>
        <InlineTextEditor
          v-if="editingId === record.id"
          :text="record.editedText"
          :original="record.originalText"
          :saving="savingEdit"
          @save="text => saveEdit(record, text)"
          @cancel="editingId = null"
        >
          <template #preview="{ draft }">
            <div v-if="draft !== record.originalText" class="edit-preview">
              <div class="preview-label">{{ t('postEdit.changesFromOriginal') }}</div>
              <CorrectionDiff
                :diff="diffText(record.originalText, draft)"
                :before-label="t('postEdit.original')"
                :after-label="t('postEdit.edited')"
              />
            </div>
          </template>
        </InlineTextEditor>
        <template v-else-if="diffs.get(record.id)">
          <CorrectionDiff
            :diff="diffs.get(record.id)!.diff"
            :before-label="t('postEdit.original')"
            :after-label="t('postEdit.edited')"
            :context="diffs.get(record.id)!.collapsible && !expanded.has(record.id) ? COLLAPSED_CONTEXT : undefined"
          />
          <footer v-if="diffs.get(record.id)!.collapsible" class="record-footer">
            <NButton text size="tiny" @click="toggle(record.id)">
              {{ t(expanded.has(record.id) ? 'postEdit.collapse' : 'postEdit.expand') }}
            </NButton>
          </footer>
        </template>
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
.record-meta { justify-content: space-between; min-height: 24px; margin-bottom: 10px; }
.meta-main { min-width: 0; }
.record-meta time { font-size: var(--font-sm); font-variant-numeric: tabular-nums; font-weight: 600; }
.app-name { color: var(--color-text-tertiary); font-size: var(--font-xs); max-width: 260px; overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
.record-actions { display: inline-flex; align-items: center; gap: 14px; flex-shrink: 0; }
.more-button { width: 24px; padding: 0; color: var(--color-text-secondary); }
.edit-preview { margin-top: 10px; padding: 10px 12px; border-radius: 6px; background: var(--color-bg-secondary); }
.preview-label { margin-bottom: 4px; font-size: var(--font-xs); color: var(--color-text-tertiary); }
.record-footer { display: flex; justify-content: flex-end; margin-top: 6px; }
</style>
