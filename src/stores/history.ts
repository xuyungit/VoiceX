import { defineStore, acceptHMRUpdate } from 'pinia'
import { ref, computed } from 'vue'
import { invoke } from '@tauri-apps/api/core'

export interface HistoryRecord {
    id: string
    timestamp: string
    text: string
    originalText: string | null
    aiCorrectionApplied: boolean
    llmInvoked: boolean
    mode: string
    durationMs: number
    audioPath: string | null
    isFinal: boolean
    errorCode: number
    sourceDeviceId: string | null
    sourceDeviceName: string | null
    asrModelName: string | null
    llmModelName: string | null
}

export interface UsageStats {
    totalDurationMs: number
    totalCharacters: number
    llmCorrectionCount: number
}

export const useHistoryStore = defineStore('history', () => {
    const records = ref<HistoryRecord[]>([])
    const stats = ref<UsageStats>({
        totalDurationMs: 0,
        totalCharacters: 0,
        llmCorrectionCount: 0
    })
    const localStats = ref<UsageStats>({
        totalDurationMs: 0,
        totalCharacters: 0,
        llmCorrectionCount: 0
    })
    const isLoading = ref(false)
    const hasMore = ref(true)
    const pageSize = 20

    // Computed: group records by date
    const recordsByDate = computed(() => {
        const groups: Record<string, HistoryRecord[]> = {}

        for (const record of records.value) {
            const date = new Date(record.timestamp).toLocaleDateString('zh-CN', {
                year: 'numeric',
                month: 'long',
                day: 'numeric'
            })
            if (!groups[date]) {
                groups[date] = []
            }
            groups[date].push(record)
        }

        return groups
    })

    const formatStats = (source: UsageStats) => {
        const totalDurationMs = source?.totalDurationMs ?? 0
        const totalCharacters = source?.totalCharacters ?? 0
        const totalMinutes = Math.floor(totalDurationMs / 60000)
        const hours = Math.floor(totalMinutes / 60)
        const minutes = totalMinutes % 60

        return {
            totalTime: hours > 0 ? `${hours}小时${minutes}分钟` : `${minutes}分钟`,
            totalCharacters: totalCharacters.toLocaleString(),
            averageSpeed: totalMinutes > 0
                ? Math.round(totalCharacters / totalMinutes)
                : 0
        }
    }

    // Computed: formatted stats
    const formattedStats = computed(() => formatStats(stats.value))
    const formattedLocalStats = computed(() => formatStats(localStats.value))

    async function loadStats() {
        try {
            const result = await invoke<UsageStats>('get_usage_stats')
            stats.value = result
        } catch (error) {
            console.error('Failed to load stats:', error)
        }

        try {
            const result = await invoke<UsageStats>('get_local_usage_stats')
            localStats.value = result
        } catch (error) {
            console.error('Failed to load local stats:', error)
        }
    }

    async function loadHistory(reset = false) {
        if (isLoading.value) return
        if (!reset && !hasMore.value) return

        try {
            isLoading.value = true
            const offset = reset ? 0 : records.value.length

            const result = await invoke<HistoryRecord[]>('get_history', {
                limit: pageSize,
                offset
            })

            // Normalize empty audio paths to null so UI buttons stay enabled only when playable
            for (const item of result) {
                if (item.audioPath !== null && item.audioPath.trim() === '') {
                    item.audioPath = null
                }
            }

            if (reset) {
                records.value = result
            } else {
                records.value.push(...result)
            }

            hasMore.value = result.length === pageSize
        } catch (error) {
            console.error('Failed to load history:', error)
        } finally {
            isLoading.value = false
        }
    }

    async function deleteRecord(id: string) {
        try {
            await invoke('delete_history_record', { id })
            records.value = records.value.filter(r => r.id !== id)
            await loadStats()
        } catch (error) {
            console.error('Failed to delete record:', error)
        }
    }

    async function copyText(text: string) {
        try {
            await navigator.clipboard.writeText(text)
        } catch (error) {
            console.error('Failed to copy text:', error)
        }
    }

    const hasOriginal = (record: HistoryRecord) => {
        return !!(record.originalText && record.originalText.length > 0)
    }

    return {
        records,
        stats,
        localStats,
        isLoading,
        hasMore,
        recordsByDate,
        formattedStats,
        formattedLocalStats,
        loadStats,
        loadHistory,
        deleteRecord,
        copyText,
        hasOriginal
    }
})

if (import.meta.hot) {
    import.meta.hot.accept(acceptHMRUpdate(useHistoryStore, import.meta.hot))
}
