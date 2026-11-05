<script setup lang="ts">
import { computed, onMounted, onBeforeUnmount, watch } from 'vue'
import { NConfigProvider, darkTheme, NMessageProvider, NDialogProvider, type GlobalThemeOverrides } from 'naive-ui'
import { listen } from '@tauri-apps/api/event'
import Sidebar from './components/Sidebar.vue'
import PageErrorBoundary from './components/PageErrorBoundary.vue'
import type { ResolvedLocale, UiLanguage } from './i18n'
import { applyUiTheme, isDark } from './theme'
import { resolveLocale, setLocale } from './i18n'
import { useSettingsStore } from './stores/settings'
import { useHistoryStore } from './stores/history'
import { useSyncStore, type SyncStateResponse } from './stores/sync'

// Naive UI derives hover/alpha shades from these values, so they must be literal colors
// (not CSS variables). Keep them in sync with styles/variables.css.
const sharedCommon: GlobalThemeOverrides['common'] = {
  fontFamily: "-apple-system, BlinkMacSystemFont, 'Segoe UI', 'PingFang SC', 'Hiragino Sans GB', 'Microsoft YaHei', sans-serif",
  fontFamilyMono: "ui-monospace, 'SF Mono', Menlo, Consolas, monospace",
  fontSize: '13px',
  fontSizeSmall: '13px',
  fontSizeMedium: '13px',
  borderRadius: '6px',
}

const darkOverrides: GlobalThemeOverrides = {
  common: {
    ...sharedCommon,
    primaryColor: '#4f8cff',
    primaryColorHover: '#6b9fff',
    primaryColorPressed: '#3b7cf0',
    primaryColorSuppl: '#6b9fff',
    bodyColor: '#1b1c1f',
    cardColor: '#232428',
    modalColor: '#232428',
    popoverColor: '#2b2c31',
    inputColor: 'rgba(255, 255, 255, 0.06)',
  },
}

const lightOverrides: GlobalThemeOverrides = {
  common: {
    ...sharedCommon,
    primaryColor: '#2563eb',
    primaryColorHover: '#3b82f6',
    primaryColorPressed: '#1d4ed8',
    primaryColorSuppl: '#3b82f6',
  },
}

const naiveTheme = computed(() => (isDark.value ? darkTheme : null))
const themeOverrides = computed(() => (isDark.value ? darkOverrides : lightOverrides))
import { usePostEditStore } from './stores/postEdit'
import { getDefaultPrompt, isBuiltInDefaultPrompt } from './utils/llmPrompts'

const settingsStore = useSettingsStore()
const historyStore = useHistoryStore()
const syncStore = useSyncStore()
// Loaded up front so History shows its tab bar on first render when enabled.
const postEditStore = usePostEditStore()
let unlistenHistory: (() => void) | null = null
let unlistenSync: (() => void) | null = null
let unlistenLocale: (() => void) | null = null
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

async function applyUiLanguage(preference: UiLanguage) {
  const locale = await resolveLocale(preference)
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
    syncStore.loadState(),
    postEditStore.load()
  ])
  await Promise.all([
    applyUiLanguage(settingsStore.settings.uiLanguage),
    applyUiTheme(settingsStore.settings.uiTheme),
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

  unlistenLocale = await listen<{ locale?: ResolvedLocale }>('ui:locale-changed', (event) => {
    const nextLocale = event.payload?.locale
    if (nextLocale === 'zh-CN' || nextLocale === 'en-US') {
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

watch(
  () => settingsStore.settings.uiTheme,
  (value) => {
    applyUiTheme(value)
  }
)

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
  <NConfigProvider :theme="naiveTheme" :theme-overrides="themeOverrides">
    <NMessageProvider>
      <NDialogProvider>
        <div class="app-container">
          <Sidebar />
          <main class="main-content">
            <div class="content-body">
              <div class="page-shell">
                <router-view v-slot="{ Component, route }">
                  <PageErrorBoundary :route-key="route.fullPath">
                    <transition name="fade" mode="out-in">
                      <component :is="Component" />
                    </transition>
                  </PageErrorBoundary>
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

.content-body {
  flex: 1;
  min-height: 0;
  overflow-y: auto;
  padding: 36px 32px 32px;
}

.page-shell {
  max-width: 1080px;
}
</style>
