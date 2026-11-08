<script setup lang="ts">
import { computed, ref, onMounted } from 'vue'
import { invoke } from '@tauri-apps/api/core'
import { NButton, NSwitch, useMessage } from 'naive-ui'
import { useI18n } from 'vue-i18n'
import { useSettingsStore } from '../stores/settings'
import { useUpdatesStore, UPDATE_ERROR_CODES } from '../stores/updates'
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
const updatesStore = useUpdatesStore()
const message = useMessage()
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

function formatTime(seconds: number) {
  return new Date(seconds * 1000).toLocaleString(locale.value, { hour12: false })
}

const builtAtLabel = computed(() => {
  const seconds = Number(builtAt.value)
  if (!Number.isFinite(seconds) || seconds <= 0) return '—'
  return formatTime(seconds)
})

const autoCheckUpdates = computed({
  get: () => settingsStore.settings.autoCheckUpdates,
  set: (v: boolean) => settingsStore.updateSetting('autoCheckUpdates', v)
})

const update = computed(() => updatesStore.status)
const available = computed(() => update.value?.available ?? null)

const updateStatusLabel = computed(() => {
  const status = update.value
  if (!status) return '—'
  if (status.phase === 'checking') return t('about.updateChecking')
  if (status.available) return t('about.updateAvailable', { version: status.available.version })
  if (status.phase === 'upToDate') return t('about.updateUpToDate')
  if (status.phase === 'error') return t('about.updateCheckFailed')
  return t('about.updateNotChecked')
})

const installProgressLabel = computed(() => {
  const status = update.value
  if (!status) return ''
  if (status.phase === 'installing') return t('about.updateInstalling')
  if (status.phase !== 'downloading') return ''
  if (status.total) {
    const percent = Math.min(100, Math.floor((status.downloaded / status.total) * 100))
    return t('about.updateDownloadingPercent', { percent })
  }
  return t('about.updateDownloadingMb', { mb: (status.downloaded / 1048576).toFixed(1) })
})

// The backend's own refusals arrive as codes; updater failures also land in
// the status (shown under the row), so they need no toast.
function reportActionError(error: unknown) {
  const code = String(error)
  if ((UPDATE_ERROR_CODES as readonly string[]).includes(code)) {
    message.warning(t(`about.updateErrors.${code}`))
  } else {
    console.error('[updates]', error)
  }
}

async function checkForUpdate() {
  try {
    await updatesStore.check()
  } catch (error) {
    reportActionError(error)
  }
}

async function installUpdate() {
  try {
    await updatesStore.install()
  } catch (error) {
    reportActionError(error)
  }
}

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

    <SettingsSection :title="t('about.updates')">
      <SettingRow v-if="!update?.enabled" :label="t('about.updatesOfficialOnly')" />
      <template v-else>
        <SettingRow :label="updateStatusLabel">
          <template #status>
            <div v-if="update.error" class="update-error">{{ update.error }}</div>
            <div v-if="update.lastCheckedAt">{{ t('about.updateLastChecked', { time: formatTime(update.lastCheckedAt) }) }}</div>
          </template>
          <NButton size="small" :loading="update.phase === 'checking'" :disabled="updatesStore.working" @click="checkForUpdate">
            {{ t('about.updateCheckNow') }}
          </NButton>
        </SettingRow>
        <SettingRow v-if="available" :label="t('about.updateNotesTitle', { version: available.version })" stacked>
          <template #status>
            <span v-if="available.publishedAt">{{ t('about.updatePublished', { time: formatTime(available.publishedAt) }) }}</span>
          </template>
          <div class="update-body">
            <div v-if="available.notes" class="update-notes">{{ available.notes }}</div>
            <div class="update-actions">
              <span v-if="installProgressLabel" class="update-progress">{{ installProgressLabel }}</span>
              <NButton type="primary" size="small" :disabled="updatesStore.working" @click="installUpdate">
                {{ t('about.updateInstall') }}
              </NButton>
            </div>
          </div>
        </SettingRow>
        <SettingRow :label="t('about.updateAutoCheck')" :help="t('about.updateAutoCheckHint')">
          <NSwitch v-model:value="autoCheckUpdates" />
        </SettingRow>
      </template>
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

.update-error {
  color: var(--color-error);
  user-select: text;
}

.update-body {
  display: flex;
  flex-direction: column;
  gap: 10px;
  width: 100%;
}

.update-notes {
  max-height: 220px;
  overflow-y: auto;
  padding: 10px 12px;
  border-radius: 6px;
  background: var(--color-bg-tertiary);
  color: var(--color-text-secondary);
  font-size: var(--font-sm);
  line-height: 1.5;
  white-space: pre-wrap;
  user-select: text;
}

.update-actions {
  display: flex;
  align-items: center;
  justify-content: flex-end;
  gap: 12px;
}

.update-progress {
  color: var(--color-text-secondary);
  font-variant-numeric: tabular-nums;
}

.build-value.mono {
  font-family: var(--font-mono);
  font-size: var(--font-sm);
}
</style>
