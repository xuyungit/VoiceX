import { defineStore } from 'pinia'
import { ref } from 'vue'
import { invoke } from '@tauri-apps/api/core'

export interface SyncState {
    lastSeq: number
    lastSyncAt: string | null
    lastError: string | null
    status: string | null
    currentServerId?: string | null
    currentAccountId?: string | null
}

export interface SyncStateResponse {
    state: SyncState
    deviceId: string
}

export const useSyncStore = defineStore('sync', () => {
    const state = ref<SyncState>({
        lastSeq: 0,
        lastSyncAt: null,
        lastError: null,
        status: null
    })
    const deviceId = ref('')
    const isLoading = ref(false)

    async function loadState() {
        try {
            isLoading.value = true
            const result = await invoke<SyncStateResponse>('get_sync_state')
            state.value = result.state
            deviceId.value = result.deviceId
        } catch (error) {
            console.error('Failed to load sync state:', error)
        } finally {
            isLoading.value = false
        }
    }

    function updateFromEvent(payload: SyncStateResponse) {
        state.value = payload.state
        deviceId.value = payload.deviceId
    }

    async function syncNow() {
        try {
            await invoke('sync_now')
        } catch (error) {
            console.error('Failed to trigger sync:', error)
        }
    }

    return {
        state,
        deviceId,
        isLoading,
        loadState,
        updateFromEvent,
        syncNow
    }
})
