import { computed, ref } from 'vue'
import { defineStore } from 'pinia'
import { invoke } from '@tauri-apps/api/core'
import type { PostEditMode, PostEditSettings } from '../types/postEdit'

export const usePostEditStore = defineStore('post-edit', () => {
  const settings = ref<PostEditSettings | null>(null)
  const saving = ref(false)
  const error = ref<string | null>(null)
  let revision = 0

  async function load() {
    if (saving.value) return
    const request = ++revision
    try {
      const result = await invoke<PostEditSettings>('get_post_edit_settings')
      if (request === revision) {
        settings.value = result
        error.value = null
      }
    } catch (e) {
      if (request === revision) error.value = String(e)
    }
  }

  async function setMode(mode: PostEditMode) {
    if (saving.value) return
    ++revision
    saving.value = true
    try {
      settings.value = await invoke<PostEditSettings>('set_post_edit_mode', { mode })
      error.value = null
    } catch (e) {
      error.value = String(e)
    } finally {
      saving.value = false
    }
  }

  const enabled = computed(() => settings.value?.mode === 'records')
  const setEnabled = (value: boolean) => setMode(value ? 'records' : 'off')

  return { settings, saving, error, enabled, load, setMode, setEnabled }
})
