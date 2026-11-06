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
    errorMessage: string | null
    sourceDeviceId: string | null
    sourceDeviceName: string | null
    asrModelName: string | null
    llmModelName: string | null
    /** The user's final version when it differs from `text` (VoiceX output). */
    editedText: string | null
}

/** The text a dictation ended up as: the user's edit, else VoiceX's output. */
export const finalText = (record: HistoryRecord) => record.editedText ?? record.text

export interface TtsCounters {
    readCount: number
    translateCount: number
    characters: number
    durationMs: number
    llmCount: number
}

export interface UsageStats {
    totalDurationMs: number
    totalCharacters: number
    llmCorrectionCount: number
    totalRecordingCount: number
    tts: TtsCounters
}

const emptyTtsCounters = (): TtsCounters => ({
    readCount: 0,
    translateCount: 0,
    characters: 0,
    durationMs: 0,
    llmCount: 0
})

export const useHistoryStore = defineStore('history', () => {
    const records = ref<HistoryRecord[]>([])
    const stats = ref<UsageStats>({
        totalDurationMs: 0,
        totalCharacters: 0,
        llmCorrectionCount: 0,
        totalRecordingCount: 0,
        tts: emptyTtsCounters()
    })
    const localStats = ref<UsageStats>({
        totalDurationMs: 0,
        totalCharacters: 0,
        llmCorrectionCount: 0,
        totalRecordingCount: 0,
        tts: emptyTtsCounters()
    })
    const isLoading = ref(false)
    const loadError = ref<string | null>(null)
    const hasMore = ref(true)
    const pageSize = 20
    let refreshQueued = false

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
        if (isLoading.value) {
            if (reset) refreshQueued = true
            return
        }
        if (!reset && !hasMore.value) return

        try {
            isLoading.value = true
            const offset = reset ? 0 : records.value.length
            const limit = reset ? Math.max(pageSize, records.value.length) : pageSize

            const result = await invoke<HistoryRecord[]>('get_history', {
                limit,
                offset
            })

            const existing = new Map(records.value.map(record => [record.id, record]))
            // Normalize empty audio paths to null so UI buttons stay enabled only when playable
            const updated = result.map(item => {
                if (item.audioPath !== null && item.audioPath.trim() === '') {
                    item.audioPath = null
                }
                // Open details hold this object too; keep them current after an observed edit.
                const record = existing.get(item.id)
                return record ? Object.assign(record, item) : item
            })

            if (reset) {
                records.value = updated
            } else {
                records.value.push(...updated)
            }

            hasMore.value = result.length === limit
            loadError.value = null
        } catch (error) {
            console.error('Failed to load history:', error)
            loadError.value = String(error)
        } finally {
            isLoading.value = false
            if (refreshQueued) {
                refreshQueued = false
                await loadHistory(true)
            }
        }
    }

    async function deleteRecord(id: string) {
        await invoke('delete_history_record', { id })
        records.value = records.value.filter(r => r.id !== id)
        await loadStats()
    }

    /** Saves a dictation's final text; returns once the change is stored. */
    async function setFinalText(id: string, text: string) {
        const result = await invoke<{ editedText: string | null }>('set_history_final_text', { recordId: id, text })
        const record = records.value.find(r => r.id === id)
        if (record) record.editedText = result.editedText
    }

    async function copyText(text: string) {
        await navigator.clipboard.writeText(text)
    }

    const hasOriginal = (record: HistoryRecord) => {
        return !!(record.originalText && record.originalText.length > 0)
    }

    return {
        records,
        stats,
        localStats,
        isLoading,
        loadError,
        hasMore,
        recordsByDate,
        formattedStats,
        formattedLocalStats,
        loadStats,
        loadHistory,
        deleteRecord,
        setFinalText,
        copyText,
        hasOriginal
    }
})

if (import.meta.hot) {
    import.meta.hot.accept(acceptHMRUpdate(useHistoryStore, import.meta.hot))
}
