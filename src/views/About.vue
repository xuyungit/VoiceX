<script setup lang="ts">
import { computed, ref, onMounted } from 'vue'
import { invoke } from '@tauri-apps/api/core'
import { NSwitch } from 'naive-ui'
import { useI18n } from 'vue-i18n'
import { useSettingsStore } from '../stores/settings'
import SettingsSection from '../components/settings/SettingsSection.vue'
import SettingRow from '../components/settings/SettingRow.vue'

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
  <div class="page settings-page">
    <div class="page-header">
      <h1 class="page-title">VoiceX</h1>
    </div>

    <SettingsSection :title="t('about.buildInfo')">
      <SettingRow :label="t('about.version')"><span class="build-value">{{ version }}</span></SettingRow>
      <SettingRow :label="t('about.build')"><span class="build-value">{{ profileLabel }}</span></SettingRow>
      <SettingRow :label="t('about.commit')"><span class="build-value mono">{{ commitLabel }}</span></SettingRow>
      <SettingRow :label="t('about.built')"><span class="build-value">{{ builtAtLabel }}</span></SettingRow>
    </SettingsSection>

    <SettingsSection :title="t('about.diagnostics')">
      <SettingRow :label="t('about.showDiagnostics')" :help="t('about.diagnosticsHint')">
        <NSwitch v-model:value="enableDiagnostics" />
      </SettingRow>
    </SettingsSection>
  </div>
</template>

<style scoped>
.build-value {
  color: var(--color-text-secondary);
  font-variant-numeric: tabular-nums;
  user-select: text;
}

.build-value.mono {
  font-family: var(--font-mono);
  font-size: var(--font-sm);
}
</style>
