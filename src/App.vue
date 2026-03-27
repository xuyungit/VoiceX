<script setup lang="ts">
import { onMounted, onBeforeUnmount, ref, watch } from 'vue'
import { NConfigProvider, darkTheme, NMessageProvider, NDialogProvider } from 'naive-ui'
import { listen } from '@tauri-apps/api/event'
import Sidebar from './components/Sidebar.vue'
import LanguageSwitcher from './components/LanguageSwitcher.vue'
import type { ResolvedLocale, UiLanguage } from './i18n'
import { resolveLocale, setLocale } from './i18n'
import { useSettingsStore } from './stores/settings'
import { useHistoryStore } from './stores/history'
import { useSyncStore, type SyncStateResponse } from './stores/sync'
import { getDefaultPrompt, isBuiltInDefaultPrompt } from './utils/llmPrompts'

const settingsStore = useSettingsStore()
const historyStore = useHistoryStore()
const syncStore = useSyncStore()
let unlistenHistory: (() => void) | null = null
let unlistenSync: (() => void) | null = null
let unlistenLocale: (() => void) | null = null
let historyRefreshTimer: number | null = null
let historyRefreshQueued = false
const resolvedLocale = ref<ResolvedLocale>('en-US')

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

async function applyUiLanguage(preference: UiLanguage) {
  const locale = await resolveLocale(preference)
  resolvedLocale.value = locale
  setLocale(locale)
  syncDefaultPrompts(locale)
}

function syncDefaultPrompts(locale: ResolvedLocale) {
  const assistantPrompt = settingsStore.settings.llmPromptTemplate
  if (isBuiltInDefaultPrompt('assistant', assistantPrompt)) {
    const nextPrompt = getDefaultPrompt('assistant', locale)
    if (assistantPrompt !== nextPrompt) {
      settingsStore.updateSetting('llmPromptTemplate', nextPrompt)
    }
  }

  const translationPrompt = settingsStore.settings.translationPromptTemplate
  if (isBuiltInDefaultPrompt('translation', translationPrompt)) {
    const nextPrompt = getDefaultPrompt('translation', locale)
    if (translationPrompt !== nextPrompt) {
      settingsStore.updateSetting('translationPromptTemplate', nextPrompt)
    }
  }
}

onMounted(async () => {
  // Load initial data
  await Promise.all([
    settingsStore.loadSettings(),
    historyStore.loadStats(),
    syncStore.loadState()
  ])
  await applyUiLanguage(settingsStore.settings.uiLanguage)
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

  unlistenLocale = await listen<{ locale?: ResolvedLocale }>('ui:locale-changed', (event) => {
    const nextLocale = event.payload?.locale
    if (nextLocale === 'zh-CN' || nextLocale === 'en-US') {
      resolvedLocale.value = nextLocale
      setLocale(nextLocale)
    }
  })
})

watch(
  () => settingsStore.settings.uiLanguage,
  (value) => {
    applyUiLanguage(value)
  }
)

function updateUiLanguage(value: UiLanguage) {
  settingsStore.updateSetting('uiLanguage', value)
}

onBeforeUnmount(() => {
  if (unlistenHistory) {
    unlistenHistory()
    unlistenHistory = null
  }
  if (unlistenSync) {
    unlistenSync()
    unlistenSync = null
  }
  if (unlistenLocale) {
    unlistenLocale()
    unlistenLocale = null
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
              <div class="header-shell">
                <div class="header-spacer"></div>
                <div class="header-actions no-drag">
                  <LanguageSwitcher
                    :model-value="settingsStore.settings.uiLanguage"
                    :resolved-locale="resolvedLocale"
                    @update:model-value="updateUiLanguage"
                  />
                </div>
              </div>
            </div>
            <div class="content-body">
              <div class="page-shell">
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
  height: 56px;
  flex-shrink: 0;
  border-bottom: 1px solid var(--color-divider);
  background:
    linear-gradient(180deg, rgba(255, 255, 255, 0.03), transparent),
    rgba(16, 18, 22, 0.72);
  backdrop-filter: blur(14px);
}

.header-shell {
  max-width: 1120px;
  height: 100%;
  margin: 0 auto;
  padding: 0 var(--spacing-xl);
  display: flex;
  align-items: center;
  justify-content: space-between;
  gap: var(--spacing-lg);
}

.header-spacer {
  flex: 1;
  min-width: 140px;
}

.header-actions {
  display: flex;
  align-items: center;
}

.content-body {
  flex: 1;
  min-height: 0;
  overflow-y: auto;
  height: calc(100vh - 56px);
  padding: 0 var(--spacing-xl) var(--spacing-xl);
}

.page-shell {
  max-width: 1120px;
  margin: 0 auto;
  padding: var(--spacing-sm) 0 var(--spacing-2xl);
}

@media (max-width: 960px) {
  .content-header {
    height: 52px;
  }

  .header-shell {
    padding: 0 var(--spacing-md);
  }

  .content-body {
    height: calc(100vh - 52px);
    padding: 0 var(--spacing-md) var(--spacing-lg);
  }
}


</style>
