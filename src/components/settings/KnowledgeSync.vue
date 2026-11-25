<script setup lang="ts">
import { onBeforeUnmount, onMounted, ref, watch } from 'vue'
import { NButton, NSwitch, useMessage } from 'naive-ui'
import { invoke } from '@tauri-apps/api/core'
import { listen, type UnlistenFn } from '@tauri-apps/api/event'
import { useI18n } from 'vue-i18n'
import { useSettingsStore } from '../../stores/settings'
import SettingsSection from './SettingsSection.vue'
import SettingRow from './SettingRow.vue'

const props = defineProps<{ spaceId: string }>()
const kinds = [{ name: 'dictionary', bit: 1 }, { name: 'rules', bit: 2 }, { name: 'prompts', bit: 4 }] as const
type Domain = typeof kinds[number]['name']
type Version = { id: string; value: null | { prompt: string } | { rules: Array<{ rule: { keyword: string; replacement: string; enabled: boolean; matchMode: string }; position: number }> } }
type Conflict = { domain: Domain; key: string; versions: Version[] }
type Status = { spaceId: string | null; scopes: number; conflicts: Conflict[] }
const status = ref<Status>({ spaceId: null, scopes: 0, conflicts: [] })
const busy = ref(false)
const { t } = useI18n()
const message = useMessage()
const settings = useSettingsStore()
let unlisten: UnlistenFn | undefined
let disposed = false
let request = 0
async function reload() {
  const id = ++request
  try {
    const result = await invoke<Status>('sync_v2_knowledge_status')
    if (!disposed && id === request && result.spaceId === props.spaceId) status.value = result
  } catch (error) { message.error(String(error)) }
}
async function change(command: string, args: Record<string, unknown>) {
  busy.value = true
  try {
    await settings.forceSaveSettings()
    await invoke(command, { ...args, spaceId: props.spaceId })
  } catch (error) { message.error(String(error)) }
  finally { await reload(); busy.value = false }
}
function title(c: Conflict) {
  return c.domain === 'prompts' ? t('sync.knowledgePrompt.' + c.key) : c.key
}
function description(v: Version) {
  if (v.value === null) return t('sync.knowledgeRemoved')
  if ('prompt' in v.value) return v.value.prompt
  const modes: Record<string, string> = { exact: 'exactMatch', contains: 'contains', regex: 'regex' }
  return v.value.rules.map(r => '#' + (r.position + 1) + ' · ' + t('postProcessing.' + modes[r.rule.matchMode]) + ' · ' + r.rule.keyword + ' → ' + r.rule.replacement + (r.rule.enabled ? '' : ' (' + t('sync.knowledgeDisabled') + ')')).join('\n')
}
onMounted(async () => {
  const stop = await listen('sync_v2:knowledge', reload)
  if (disposed) { stop(); return }
  unlisten = stop
  await reload()
})
watch(() => props.spaceId, reload)
onBeforeUnmount(() => { disposed = true; unlisten?.() })
</script>
<template>
  <SettingsSection :title="t('sync.knowledgeTitle')" :help="t('sync.knowledgeHelp')">
    <SettingRow v-for="kind in kinds" :key="kind.name" :label="t('sync.knowledgeLabels.' + kind.name)">
      <NSwitch :aria-label="t('sync.knowledgeLabels.' + kind.name)" :value="Boolean(status.scopes & kind.bit)" :disabled="busy || status.spaceId !== spaceId"
        @update:value="enabled => change('sync_v2_set_knowledge_scope', { domain: kind.name, enabled })" />
    </SettingRow>
    <div v-for="conflict in status.conflicts" :key="conflict.domain + conflict.key" class="knowledge-conflict">
      <strong>{{ title(conflict) }}</strong>
      <p>{{ t(conflict.domain === 'rules' ? 'sync.ruleConflictHelp' : 'sync.promptConflictHelp') }}</p>
      <div v-for="version in conflict.versions" :key="version.id">
        <pre>{{ description(version) }}</pre>
        <NButton size="small" :disabled="busy" @click="change('sync_v2_resolve_knowledge', { domain: conflict.domain,
          key: conflict.key, selected: version.id, expected: conflict.versions.map(v => v.id) })">{{ t('history.editConflictUseVersion') }}</NButton>
      </div>
    </div>
  </SettingsSection>
</template>
<style scoped>
.knowledge-conflict { padding: 12px 0; border-top: 1px solid var(--color-border); }
.knowledge-conflict pre { white-space: pre-wrap; overflow-wrap: anywhere; max-height: 14rem; overflow: auto; }
</style>
