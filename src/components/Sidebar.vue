<script setup lang="ts">
import { useRouter, useRoute } from 'vue-router'
import { computed, h } from 'vue'
import { useI18n } from 'vue-i18n'
import { NDropdown, NTooltip } from 'naive-ui'
import LanguageSwitcher from './LanguageSwitcher.vue'
import { useSettingsStore } from '../stores/settings'
import type { UiTheme } from '../theme'
import type { UiLanguage } from '../i18n'

const router = useRouter()
const route = useRoute()
const { t } = useI18n()
const settingsStore = useSettingsStore()

// Material icons: brightness_auto-ish half circle, light_mode, dark_mode.
const THEME_ICONS: Record<UiTheme, string> = {
  system: 'M12 2a10 10 0 1 0 0 20 10 10 0 0 0 0-20zm0 18V4a8 8 0 0 1 0 16z',
  light: 'M12 7a5 5 0 1 0 0 10 5 5 0 0 0 0-10zM2 13h2a1 1 0 0 0 0-2H2a1 1 0 0 0 0 2zm18 0h2a1 1 0 0 0 0-2h-2a1 1 0 0 0 0 2zM11 2v2a1 1 0 0 0 2 0V2a1 1 0 0 0-2 0zm0 18v2a1 1 0 0 0 2 0v-2a1 1 0 0 0-2 0zM5.99 4.58a1 1 0 0 0-1.41 1.41l1.06 1.06a1 1 0 0 0 1.41-1.41L5.99 4.58zm12.37 12.37a1 1 0 0 0-1.41 1.41l1.06 1.06a1 1 0 0 0 1.41-1.41l-1.06-1.06zm1.06-10.96a1 1 0 0 0-1.41-1.41l-1.06 1.06a1 1 0 0 0 1.41 1.41l1.06-1.06zM7.05 18.36a1 1 0 0 0-1.41-1.41l-1.06 1.06a1 1 0 0 0 1.41 1.41l1.06-1.06z',
  dark: 'M12 3a9 9 0 1 0 9 9c0-.46-.04-.92-.1-1.36a5.39 5.39 0 0 1-4.4 2.26 5.4 5.4 0 0 1-3.14-9.8c-.44-.06-.9-.1-1.36-.1z',
}

function themeIcon(theme: UiTheme) {
  return () => h('svg', { viewBox: '0 0 24 24', fill: 'currentColor', width: 16, height: 16 }, [h('path', { d: THEME_ICONS[theme] })])
}

const themeLabels = computed<Record<UiTheme, string>>(() => ({
  system: t('appearance.themeSystem'),
  light: t('appearance.themeLight'),
  dark: t('appearance.themeDark'),
}))

const themeOptions = computed(() =>
  (['system', 'light', 'dark'] as UiTheme[]).map((key) => ({
    key,
    label: themeLabels.value[key],
    icon: themeIcon(key),
  }))
)

const currentTheme = computed(() => settingsStore.settings.uiTheme)

function setTheme(key: string | number) {
  settingsStore.updateSetting('uiTheme', key as UiTheme)
}

function setLanguage(value: UiLanguage) {
  settingsStore.updateSetting('uiLanguage', value)
}

interface NavItem {
  path: string
  name: string
  icon: string
  labelKey: string
}

interface NavGroup {
  label?: string
  items: NavItem[]
}

const navGroups: NavGroup[] = [
  {
    items: [
      { path: '/overview', name: 'overview', icon: 'chart', labelKey: 'nav.overview' },
      { path: '/dictionary', name: 'dictionary', icon: 'book', labelKey: 'nav.hotwords' },
      { path: '/history', name: 'history', icon: 'history', labelKey: 'nav.history' },
    ]
  },
  {
    label: 'nav.settings',
    items: [
      { path: '/asr-settings', name: 'asr-settings', icon: 'mic', labelKey: 'nav.asrSettings' },
      { path: '/llm-settings', name: 'llm-settings', icon: 'brain', labelKey: 'nav.llmSettings' },
      { path: '/input-settings', name: 'input-settings', icon: 'keyboard', labelKey: 'nav.inputSettings' },
      { path: '/reading-settings', name: 'reading-settings', icon: 'speaker', labelKey: 'nav.readingSettings' },
      { path: '/sync', name: 'sync', icon: 'sync', labelKey: 'nav.sync' },
      { path: '/post-processing', name: 'post-processing', icon: 'wand', labelKey: 'nav.postProcessing' },
    ]
  },
  {
    items: [
      { path: '/about', name: 'about', icon: 'info', labelKey: 'nav.about' },
    ]
  }
]

const currentPath = computed(() => route.path)

function navigateTo(path: string) {
  router.push(path)
}
</script>

<template>
  <nav class="sidebar">
    <div class="sidebar-header">
      <div class="app-icon">
        <svg viewBox="0 0 24 24" fill="currentColor" width="24" height="24">
          <path d="M12 14c1.66 0 3-1.34 3-3V5c0-1.66-1.34-3-3-3S9 3.34 9 5v6c0 1.66 1.34 3 3 3zm-1-9c0-.55.45-1 1-1s1 .45 1 1v6c0 .55-.45 1-1 1s-1-.45-1-1V5z"/>
          <path d="M17 11c0 2.76-2.24 5-5 5s-5-2.24-5-5H5c0 3.53 2.61 6.43 6 6.92V21h2v-3.08c3.39-.49 6-3.39 6-6.92h-2z"/>
        </svg>
      </div>
      <span class="app-name">VoiceX</span>
    </div>
    
    <div class="nav-items">
      <template v-for="(group, gi) in navGroups" :key="gi">
        <div v-if="group.label" class="nav-group-label">{{ t(group.label) }}</div>
        <div v-else-if="gi > 0" class="nav-divider" />
        <button
          v-for="item in group.items"
          :key="item.path"
          class="nav-item"
          :class="{ active: currentPath === item.path }"
          @click="navigateTo(item.path)"
        >
        <span class="nav-icon">
          <!-- Chart icon -->
          <svg v-if="item.icon === 'chart'" viewBox="0 0 24 24" fill="currentColor">
            <path d="M3 13h2v8H3v-8zm4-6h2v14H7V7zm4 3h2v11h-2V10zm4-6h2v17h-2V4z"/>
          </svg>
          <!-- History icon -->
          <svg v-else-if="item.icon === 'history'" viewBox="0 0 24 24" fill="currentColor">
            <path d="M13 3a9 9 0 0 0-9 9H1l3.89 3.89.07.14L9 12H6c0-3.87 3.13-7 7-7s7 3.13 7 7-3.13 7-7 7c-1.93 0-3.68-.79-4.94-2.06l-1.42 1.42A8.954 8.954 0 0 0 13 21a9 9 0 0 0 0-18zm-1 5v5l4.28 2.54.72-1.21-3.5-2.08V8H12z"/>
          </svg>
          <!-- Book icon -->
          <svg v-else-if="item.icon === 'book'" viewBox="0 0 24 24" fill="currentColor">
            <path d="M18 2H6c-1.1 0-2 .9-2 2v16c0 1.1.9 2 2 2h12c1.1 0 2-.9 2-2V4c0-1.1-.9-2-2-2zM6 4h5v8l-2.5-1.5L6 12V4z"/>
          </svg>
          <!-- Sync icon -->
          <svg v-else-if="item.icon === 'sync'" viewBox="0 0 24 24" fill="currentColor">
            <path d="M12 4V1L8 5l4 4V6c3.31 0 6 2.69 6 6 0 1.01-.25 1.97-.7 2.8l1.46 1.46C19.54 15.03 20 13.57 20 12c0-4.42-3.58-8-8-8zm0 14c-3.31 0-6-2.69-6-6 0-1.01.25-1.97.7-2.8L5.24 7.74C4.46 8.97 4 10.43 4 12c0 4.42 3.58 8 8 8v3l4-4-4-4v3z"/>
          </svg>
          <!-- Mic icon -->
          <svg v-else-if="item.icon === 'mic'" viewBox="0 0 24 24" fill="currentColor">
            <path d="M12 14c1.66 0 3-1.34 3-3V5c0-1.66-1.34-3-3-3S9 3.34 9 5v6c0 1.66 1.34 3 3 3zm5.91-3c-.49 0-.9.36-.98.85C16.52 14.2 14.47 16 12 16s-4.52-1.8-4.93-4.15a.998.998 0 0 0-.98-.85c-.61 0-1.09.54-1 1.14.49 3 2.89 5.35 5.91 5.78V20c0 .55.45 1 1 1s1-.45 1-1v-2.08a6.993 6.993 0 0 0 5.91-5.78c.1-.6-.39-1.14-1-1.14z"/>
          </svg>
          <!-- Brain icon -->
          <svg v-else-if="item.icon === 'brain'" viewBox="0 0 24 24" fill="currentColor">
            <path d="M20 2H4c-1.1 0-1.99.9-1.99 2L2 22l4-4h14c1.1 0 2-.9 2-2V4c0-1.1-.9-2-2-2zM9 11H7V9h2v2zm4 0h-2V9h2v2zm4 0h-2V9h2v2z"/>
          </svg>
          <!-- Keyboard icon -->
          <svg v-else-if="item.icon === 'keyboard'" viewBox="0 0 24 24" fill="currentColor">
            <path d="M20 5H4c-1.1 0-1.99.9-1.99 2L2 17c0 1.1.9 2 2 2h16c1.1 0 2-.9 2-2V7c0-1.1-.9-2-2-2zm-9 3h2v2h-2V8zm0 3h2v2h-2v-2zM8 8h2v2H8V8zm0 3h2v2H8v-2zm-1 2H5v-2h2v2zm0-3H5V8h2v2zm9 7H8v-2h8v2zm0-4h-2v-2h2v2zm0-3h-2V8h2v2zm3 3h-2v-2h2v2zm0-3h-2V8h2v2z"/>
          </svg>
          <!-- Speaker icon -->
          <svg v-else-if="item.icon === 'speaker'" viewBox="0 0 24 24" fill="currentColor">
            <path d="M3 9v6h4l5 5V4L7 9H3zm13.5 3c0-1.77-1.02-3.29-2.5-4.03v8.05c1.48-.73 2.5-2.25 2.5-4.02zM14 3.23v2.06c2.89.86 5 3.54 5 6.71s-2.11 5.85-5 6.71v2.06c4.01-.91 7-4.49 7-8.77s-2.99-7.86-7-8.77z"/>
          </svg>
          <!-- Info icon -->
          <svg v-else-if="item.icon === 'info'" viewBox="0 0 24 24" fill="currentColor">
            <path d="M12 2C6.48 2 2 6.48 2 12s4.48 10 10 10 10-4.48 10-10S17.52 2 12 2zm1 15h-2v-6h2v6zm0-8h-2V7h2v2z"/>
          </svg>
          <!-- Wand/Post-processing icon -->
          <svg v-else-if="item.icon === 'wand'" viewBox="0 0 24 24" fill="currentColor">
            <path d="M7.5 5.6L10 7L8.6 4.5L10 2L7.5 3.4L5 2L6.4 4.5L5 7L7.5 5.6M19.5 15.4L17 14L18.4 16.5L17 19L19.5 17.6L22 19L20.6 16.5L22 14L19.5 15.4M22 2L19.5 3.4L17 2L18.4 4.5L17 7L19.5 5.6L22 7L20.6 4.5L22 2M13.38 12.81L4.41 21.78C4.21 21.98 3.9 21.98 3.7 21.78L2.22 20.3C2.02 20.1 2.02 19.79 2.22 19.59L11.19 10.62C11.39 10.42 11.7 10.42 11.9 10.62L13.38 12.1C13.58 12.3 13.58 12.61 13.38 12.81Z"/>
          </svg>
        </span>
        <span class="nav-label">{{ t(item.labelKey) }}</span>
      </button>
      </template>
    </div>

    <div class="sidebar-footer">
      <LanguageSwitcher :model-value="settingsStore.settings.uiLanguage" @update:model-value="setLanguage" />
      <NDropdown trigger="click" placement="top-end" :options="themeOptions" :value="currentTheme" @select="setTheme">
        <span>
          <NTooltip placement="top" :delay="400">
            <template #trigger>
              <button type="button" class="theme-button" :aria-label="`${t('appearance.theme')}: ${themeLabels[currentTheme]}`">
                <svg viewBox="0 0 24 24" fill="currentColor"><path :d="THEME_ICONS[currentTheme]" /></svg>
              </button>
            </template>
            {{ t('appearance.theme') }}: {{ themeLabels[currentTheme] }}
          </NTooltip>
        </span>
      </NDropdown>
    </div>
  </nav>
</template>

<style scoped>
.sidebar {
  width: var(--sidebar-width);
  flex-shrink: 0;
  height: 100%;
  background-color: var(--sidebar-bg);
  display: flex;
  flex-direction: column;
  border-right: 1px solid var(--color-divider);
}

.sidebar-header {
  display: flex;
  align-items: center;
  gap: 8px;
  padding: 32px 18px 16px;
}

.app-icon {
  width: 20px;
  height: 20px;
  display: flex;
  align-items: center;
  justify-content: center;
  color: var(--color-accent);
}

.app-icon svg {
  width: 20px;
  height: 20px;
}

.app-name {
  font-size: var(--font-lg);
  font-weight: 600;
  color: var(--color-text-primary);
}

.nav-items {
  flex: 1;
  padding: 0 10px;
  display: flex;
  flex-direction: column;
  gap: 1px;
  overflow-y: auto;
}

.nav-item {
  display: flex;
  align-items: center;
  gap: 10px;
  height: 32px;
  padding: 0 10px;
  border-radius: 6px;
  color: var(--color-text-secondary);
  transition: background-color var(--transition-fast), color var(--transition-fast);
  text-align: left;
  width: 100%;
}

.nav-item:hover {
  background-color: var(--sidebar-item-hover);
  color: var(--color-text-primary);
}

.nav-item.active {
  background-color: var(--sidebar-item-active);
  color: var(--color-text-primary);
}

.nav-item.active .nav-icon {
  color: var(--color-accent);
}

.nav-icon {
  width: 18px;
  height: 18px;
  display: flex;
  align-items: center;
  justify-content: center;
  flex-shrink: 0;
}

.nav-icon svg {
  width: 16px;
  height: 16px;
}

.nav-label {
  font-size: var(--font-md);
}

.nav-group-label {
  font-size: var(--font-sm);
  color: var(--color-text-tertiary);
  padding: 14px 10px 4px;
}

.nav-divider {
  height: 1px;
  background: var(--color-divider);
  margin: 8px 10px;
}

.sidebar-footer {
  display: flex;
  align-items: center;
  justify-content: space-between;
  gap: 8px;
  padding: 10px 12px 12px;
}

.theme-button {
  width: 26px;
  height: 26px;
  display: inline-flex;
  align-items: center;
  justify-content: center;
  border-radius: 6px;
  color: var(--color-text-tertiary);
  transition: background-color var(--transition-fast), color var(--transition-fast);
}

.theme-button:hover {
  background: var(--sidebar-item-hover);
  color: var(--color-text-primary);
}

.theme-button svg {
  width: 16px;
  height: 16px;
}
</style>
