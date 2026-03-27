<script setup lang="ts">
import { computed, ref, onMounted } from 'vue'
import { invoke } from '@tauri-apps/api/core'
import { NSwitch } from 'naive-ui'
import { useI18n } from 'vue-i18n'
import { useSettingsStore } from '../stores/settings'

type BuildInfo = {
  version: string
  profile: string
  commit: string
  builtAt: string
}

const version = ref('—')
const buildProfile = ref('—')
const commit = ref('—')
const builtAt = ref('—')
const settingsStore = useSettingsStore()
const { t, locale } = useI18n()

const enableDiagnostics = computed({
  get: () => settingsStore.settings.enableDiagnostics,
  set: (v: boolean) => settingsStore.updateSetting('enableDiagnostics', v)
})

const profileLabel = computed(() => {
  if (!buildProfile.value || buildProfile.value === 'unknown') return '—'
  return buildProfile.value === 'release'
    ? 'Release'
    : buildProfile.value === 'debug'
      ? 'Debug'
      : buildProfile.value
})

const commitLabel = computed(() => {
  if (!commit.value || commit.value === 'unknown') return '—'
  return commit.value
})

const builtAtLabel = computed(() => {
  const seconds = Number(builtAt.value)
  if (!Number.isFinite(seconds) || seconds <= 0) return '—'
  return new Date(seconds * 1000).toLocaleString(locale.value, { hour12: false })
})

onMounted(async () => {
  try {
    const info = await invoke<BuildInfo>('get_build_info')
    version.value = info.version
    buildProfile.value = info.profile
    commit.value = info.commit
    builtAt.value = info.builtAt
  } catch (error) {
    console.error('Failed to load build info:', error)
  }
})
</script>

<template>
  <div class="page about-page">
    <div class="page-header">
      <h1 class="page-title">VoiceX</h1>
      <div class="page-subtitle">
        <span class="subtitle-item">
          <span class="subtitle-label">{{ t('about.version') }}</span>
          <span class="pill">{{ version }} ({{ profileLabel }})</span>
        </span>
      </div>
    </div>

    <div class="surface-card build-card">
      <div class="section-title">{{ t('about.buildInfo') }}</div>
      <div class="build-grid">
        <div class="build-row">
          <span class="muted">{{ t('about.version') }}</span>
          <span>{{ version }}</span>
        </div>
        <div class="build-row">
          <span class="muted">{{ t('about.build') }}</span>
          <span>{{ profileLabel }}</span>
        </div>
        <div class="build-row">
          <span class="muted">{{ t('about.commit') }}</span>
          <span>{{ commitLabel }}</span>
        </div>
        <div class="build-row">
          <span class="muted">{{ t('about.built') }}</span>
          <span>{{ builtAtLabel }}</span>
        </div>
      </div>
    </div>

    <div class="surface-card support-card">
      <div class="section-header">
        <div>
          <div class="section-title">{{ t('about.diagnostics') }}</div>
          <div class="section-hint">{{ t('about.diagnosticsHint') }}</div>
        </div>
        <NSwitch v-model:value="enableDiagnostics" />
      </div>
    </div>
  </div>
</template>

<style scoped>
.about-page {
  max-width: 800px;
}

.build-card {
  width: 360px;
}

.support-card {
  width: 360px;
}

.build-grid {
  display: grid;
  gap: var(--spacing-sm);
  margin-top: var(--spacing-md);
}

.build-row {
  display: flex;
  justify-content: space-between;
  font-size: var(--font-md);
}
</style>
