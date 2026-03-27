<script setup lang="ts">
import { onMounted, onBeforeUnmount } from 'vue'
import { NConfigProvider, darkTheme, NMessageProvider, NDialogProvider } from 'naive-ui'
import { listen } from '@tauri-apps/api/event'
import Sidebar from './components/Sidebar.vue'
import { useSettingsStore } from './stores/settings'
import { useHistoryStore } from './stores/history'
import { useSyncStore, type SyncStateResponse } from './stores/sync'

const settingsStore = useSettingsStore()
const historyStore = useHistoryStore()
const syncStore = useSyncStore()
let unlistenHistory: (() => void) | null = null
let unlistenSync: (() => void) | null = null
let historyRefreshTimer: number | null = null
let historyRefreshQueued = false

const SYNC_GATED_STATUSES = new Set(['connecting', 'syncing', 'reconnecting'])

async function refreshHistoryNow() {
  await Promise.all([
    historyStore.loadHistory(true),
    historyStore.loadStats()
  ])
}

function scheduleHistoryRefresh(delayMs = 300) {
  if (historyRefreshTimer !== null) {
    clearTimeout(historyRefreshTimer)
  }
  historyRefreshTimer = window.setTimeout(async () => {
    historyRefreshTimer = null
    historyRefreshQueued = false
    await refreshHistoryNow()
  }, delayMs)
}

function onHistoryUpdated() {
  const status = syncStore.state.status || ''
  if (SYNC_GATED_STATUSES.has(status)) {
    historyRefreshQueued = true
    return
  }
  scheduleHistoryRefresh()
}

function onSyncStatusChanged(nextStatus: string | null) {
  if (nextStatus === 'live' && historyRefreshQueued) {
    scheduleHistoryRefresh(0)
  }
}

onMounted(async () => {
  // Load initial data
  await Promise.all([
    settingsStore.loadSettings(),
    historyStore.loadStats(),
    syncStore.loadState()
  ])
  // Online hotword sync disabled — inline hotwords via dictionary are sufficient.
  // settingsStore.startHotwordSyncScheduler()

  // Refresh history/stats when backend signals a new record was persisted.
  unlistenHistory = await listen('history:updated', onHistoryUpdated)

  unlistenSync = await listen('sync:status', (event) => {
    const payload = event.payload as SyncStateResponse
    const nextStatus = payload?.state?.status ?? null
    syncStore.updateFromEvent(payload)
    onSyncStatusChanged(nextStatus)
  })
})

onBeforeUnmount(() => {
  if (unlistenHistory) {
    unlistenHistory()
    unlistenHistory = null
  }
  if (unlistenSync) {
    unlistenSync()
    unlistenSync = null
  }
  if (historyRefreshTimer !== null) {
    clearTimeout(historyRefreshTimer)
    historyRefreshTimer = null
  }
  // settingsStore.stopHotwordSyncScheduler()
})
</script>

<template>
  <NConfigProvider :theme="darkTheme">
    <NMessageProvider>
      <NDialogProvider>
        <div class="app-container">
          <Sidebar />
          <main class="main-content">
            <div class="content-header drag-region">
              <!-- Window title bar area for dragging -->
            </div>
            <div class="content-body">
              <div class="page-shell">
                <div class="brand">
                  <div class="brand-name">VoiceX</div>
                  <div class="brand-meta text-tertiary">跨平台语音输入</div>
                </div>
                <router-view v-slot="{ Component }">
                  <transition name="fade" mode="out-in">
                    <component :is="Component" />
                  </transition>
                </router-view>
              </div>
            </div>
          </main>
        </div>
      </NDialogProvider>
    </NMessageProvider>
  </NConfigProvider>
</template>

<style scoped>
.app-container {
  display: flex;
  height: 100vh;
  width: 100%;
  min-height: 0;
}

.main-content {
  flex: 1;
  height: 100vh;
  min-height: 0;
  display: flex;
  flex-direction: column;
  background-color: var(--color-bg-primary);
  overflow: hidden;
}

.content-header {
  height: 40px;
  flex-shrink: 0;
}

.content-body {
  flex: 1;
  min-height: 0;
  overflow-y: auto;
  height: calc(100vh - 40px);
  padding: 0 var(--spacing-xl) var(--spacing-xl);
}

.page-shell {
  max-width: 1120px;
  margin: 0 auto;
  padding: var(--spacing-xl) 0 var(--spacing-2xl);
}

.brand {
  margin-bottom: var(--spacing-xl);
}

.brand-name {
  font-size: var(--font-xl);
  font-weight: 700;
  letter-spacing: 0.01em;
}

.brand-meta {
  margin-top: 4px;
  font-size: var(--font-sm);
}
</style>
