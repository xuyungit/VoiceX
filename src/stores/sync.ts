import { defineStore } from 'pinia'
import { ref } from 'vue'
import { invoke } from '@tauri-apps/api/core'
import type { OverviewDeviceSyncInfo } from './history'

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

export interface SyncV2StatusSnapshot {
    configured: boolean
    spaceId: string | null
    serverUrl: string | null
    accountId: string | null
    deviceId: string
    deviceName: string
    status: string
    lastError: string | null
    lastSyncAt: string | null
    scopeHistory: boolean
    scopeEdits: boolean
    pendingOpsCount: number
    pendingStatsCount: number
    hasCredentials: boolean
    devices: OverviewDeviceSyncInfo[]
}

export interface PairSuccessResult {
    spaceId: string
    accountId: string
    serverId: string
    deviceId: string
    recoveryKey: string
    generatedNewRecoveryKey: boolean
}

const emptyV2Status = (): SyncV2StatusSnapshot => ({
    configured: false,
    spaceId: null,
    serverUrl: null,
    accountId: null,
    deviceId: '',
    deviceName: '',
    status: 'disabled',
    lastError: null,
    lastSyncAt: null,
    scopeHistory: false,
    scopeEdits: false,
    pendingOpsCount: 0,
    pendingStatsCount: 0,
    hasCredentials: false,
    devices: [],
})

export const useSyncStore = defineStore('sync', () => {
    const state = ref<SyncState>({
        lastSeq: 0,
        lastSyncAt: null,
        lastError: null,
        status: null
    })
    const deviceId = ref('')
    const isLoading = ref(false)
    const v2Status = ref<SyncV2StatusSnapshot>(emptyV2Status())

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

    async function loadV2Status() {
        try {
            v2Status.value = await invoke<SyncV2StatusSnapshot>('sync_v2_get_status')
            if (v2Status.value.deviceId) {
                deviceId.value = v2Status.value.deviceId
            }
        } catch (error) {
            console.error('Failed to load Sync v2 status:', error)
        }
    }

    function updateFromEvent(payload: SyncStateResponse) {
        state.value = payload.state
        deviceId.value = payload.deviceId
    }

    function updateV2FromEvent(payload: SyncV2StatusSnapshot) {
        v2Status.value = payload
        if (payload.deviceId) {
            deviceId.value = payload.deviceId
        }
    }

    async function syncNow() {
        try {
            await invoke('sync_now')
        } catch (error) {
            console.error('Failed to trigger sync:', error)
        }
    }

    async function pairV2(params: {
        serverUrl: string
        pairingCode: string
        deviceName: string
        recoveryKey?: string
        scopeHistory: boolean
        scopeEdits: boolean
    }): Promise<PairSuccessResult> {
        const res = await invoke<PairSuccessResult>('sync_v2_pair', {
            serverUrl: params.serverUrl,
            pairingCode: params.pairingCode,
            deviceName: params.deviceName,
            recoveryKey: params.recoveryKey?.trim() ? params.recoveryKey.trim() : null,
            scopeHistory: params.scopeHistory,
            scopeEdits: params.scopeEdits,
        })
        await loadV2Status()
        return res
    }

    async function updateV2Scope(params: {
        syncHistory: boolean
        syncEdits: boolean
        deviceName?: string
    }) {
        v2Status.value = await invoke<SyncV2StatusSnapshot>('sync_v2_update_scope', {
            syncHistory: params.syncHistory,
            syncEdits: params.syncEdits,
            deviceName: params.deviceName ?? null,
        })
    }

    async function getV2RecoveryKey(): Promise<string | null> {
        return await invoke<string | null>('sync_v2_get_recovery_key')
    }

    async function importV2ExistingEdits(): Promise<number> {
        const count = await invoke<number>('sync_v2_import_existing_edits')
        await loadV2Status()
        return count
    }

    async function syncV2Now() {
        v2Status.value = await invoke<SyncV2StatusSnapshot>('sync_v2_sync_now')
    }

    async function disconnectV2(clearCredentials = false) {
        v2Status.value = await invoke<SyncV2StatusSnapshot>('sync_v2_disconnect', {
            clearCredentials,
        })
    }

    return {
        state,
        deviceId,
        isLoading,
        v2Status,
        loadState,
        loadV2Status,
        updateFromEvent,
        updateV2FromEvent,
        syncNow,
        pairV2,
        updateV2Scope,
        getV2RecoveryKey,
        importV2ExistingEdits,
        syncV2Now,
        disconnectV2,
    }
})
