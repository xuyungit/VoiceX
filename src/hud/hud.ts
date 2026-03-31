// VoiceX HUD Logic (TypeScript source)
import { invoke } from '@tauri-apps/api/core';
import { listen } from '@tauri-apps/api/event';
import zhCN from '../i18n/locales/zh-CN';
import enUS from '../i18n/locales/en-US';

const statusIcon = document.getElementById('statusIcon');
const countdown = document.getElementById('countdown');
const textArea = document.getElementById('textArea');
const intentChip = document.getElementById('intentChip');
const waveformBars = document.getElementById('waveformBars');
const titleElement = document.querySelector('title');

if (!statusIcon || !countdown || !textArea || !intentChip || !waveformBars) {
    console.error('[HUD] Missing required DOM elements', {
        statusIcon: !!statusIcon,
        countdown: !!countdown,
        textArea: !!textArea,
        intentChip: !!intentChip,
        waveformBars: !!waveformBars
    });
}

const waveformBarNodes = waveformBars
    ? Array.from(waveformBars.querySelectorAll('span')) as HTMLSpanElement[]
    : [];

let currentMode: 'idle' | 'push_to_talk' | 'hands_free' | 'recognizing' | 'correcting' = 'idle';
let currentIntent: 'assistant' | 'translate_en' = 'assistant';
let hudPresentation: 'stream' | 'batch' = 'stream';
let lastActiveIcon: 'mic' | 'waveform' | 'cloud' | 'wand' = 'mic';
let partialText = '';
let lastNonEmptyText = '';
let currentLocale: 'zh-CN' | 'en-US' = 'en-US';
let currentAudioLevel = 0;
let smoothedAudioLevel = 0;
let waveformFrameId = 0;

const HUD_FONT = '12px -apple-system, BlinkMacSystemFont, "Segoe UI", "PingFang SC", "Hiragino Sans GB", "Microsoft YaHei", sans-serif';
const MAX_LINES = 2;
const ELLIPSIS = '\u2026';
const WAVEFORM_WEIGHTS = [0.42, 0.62, 0.82, 1.0, 0.82, 0.62, 0.42];
const WAVEFORM_ATTACK = 0.4;
const WAVEFORM_RELEASE = 0.15;
const WAVEFORM_JITTER = 0.04;
const WAVEFORM_GAIN = 6.5;

let measureCtx: OffscreenCanvasRenderingContext2D | CanvasRenderingContext2D | null = null;
let textAreaMaxWidth = 0;

function getMeasureCtx() {
    if (measureCtx) return measureCtx;
    if (typeof OffscreenCanvas !== 'undefined') {
        measureCtx = new OffscreenCanvas(1, 1).getContext('2d');
    } else {
        measureCtx = document.createElement('canvas').getContext('2d');
    }
    if (measureCtx) measureCtx.font = HUD_FONT;
    return measureCtx;
}

function measureText(text: string): number {
    const ctx = getMeasureCtx();
    return ctx ? ctx.measureText(text).width : text.length * 7;
}

function clamp(value: number, min: number, max: number): number {
    return Math.min(Math.max(value, min), max);
}

function updateTextAreaMaxWidth() {
    if (!textArea) return;
    const style = getComputedStyle(textArea);
    const pl = parseFloat(style.paddingLeft) || 0;
    const pr = parseFloat(style.paddingRight) || 0;
    textAreaMaxWidth = textArea.clientWidth - pl - pr;
    if (textAreaMaxWidth <= 0) textAreaMaxWidth = 230;
}

function fitText(text: string): string {
    if (!text) return text;
    const maxW = textAreaMaxWidth || 230;
    const totalBudget = (maxW - 4) * MAX_LINES;

    if (measureText(text) <= totalBudget) return text;

    const ellipsisW = measureText(ELLIPSIS);
    const budget = totalBudget - ellipsisW;

    let lo = 0;
    let hi = text.length;
    while (lo < hi) {
        const mid = (lo + hi + 1) >> 1;
        if (measureText(text.slice(-mid)) <= budget) {
            lo = mid;
        } else {
            hi = mid - 1;
        }
    }
    return lo > 0 ? ELLIPSIS + text.slice(-lo) : ELLIPSIS;
}

window.addEventListener('resize', () => {
    updateTextAreaMaxWidth();
    renderTranscript();
});

const icons: Record<string, Element | null | undefined> = {
    mic: statusIcon?.querySelector('.icon-mic') ?? null,
    waveform: statusIcon?.querySelector('.icon-waveform') ?? null,
    cloud: statusIcon?.querySelector('.icon-cloud') ?? null,
    wand: statusIcon?.querySelector('.icon-wand') ?? null
};

const hudMessages = {
    'zh-CN': zhCN.hud,
    'en-US': enUS.hud
};

function t(key: keyof typeof zhCN.hud) {
    return hudMessages[currentLocale][key];
}

function setHudLocale(locale: string | undefined) {
    currentLocale = locale === 'zh-CN' ? 'zh-CN' : 'en-US';
    document.documentElement.lang = currentLocale;
    if (titleElement) {
        titleElement.textContent = 'VoiceX HUD';
    }
    updateIntent(currentIntent);
    renderTranscript();
}

function showIcon(name: keyof typeof icons) {
    Object.values(icons).forEach((icon) => icon?.classList.remove('active'));
    icons[name]?.classList.add('active');
}

function isBatchWaveMode() {
    return hudPresentation === 'batch'
        && (currentMode === 'push_to_talk' || currentMode === 'hands_free');
}

function isCompactBatchMode() {
    return hudPresentation === 'batch';
}

function setBatchLayoutMode(batchWaveMode: boolean, compactBatchMode: boolean) {
    document.body.classList.toggle('batch-wave-mode', batchWaveMode);
    document.body.classList.toggle('compact-batch-mode', compactBatchMode);

    if (textArea) {
        textArea.hidden = compactBatchMode;
    }
    if (waveformBars) {
        waveformBars.hidden = !compactBatchMode;
    }
}

function updateStatus(mode: typeof currentMode) {
    currentMode = mode;
    document.body.classList.remove('recording', 'recognizing', 'correcting');

    switch (mode) {
        case 'push_to_talk':
            document.body.classList.add('recording');
            showIcon(isCompactBatchMode() ? 'waveform' : 'mic');
            statusIcon?.classList.add('animating');
            lastActiveIcon = isCompactBatchMode() ? 'waveform' : 'mic';
            break;
        case 'hands_free':
            document.body.classList.add('recording');
            showIcon('waveform');
            statusIcon?.classList.add('animating');
            lastActiveIcon = 'waveform';
            break;
        case 'recognizing':
            document.body.classList.add('recognizing');
            showIcon('cloud');
            statusIcon?.classList.add('animating');
            lastActiveIcon = 'cloud';
            break;
        case 'correcting':
            document.body.classList.add('correcting');
            showIcon('wand');
            statusIcon?.classList.add('animating');
            lastActiveIcon = 'wand';
            break;
        case 'idle':
        default:
            showIcon(lastActiveIcon);
            statusIcon?.classList.remove('animating');
            break;
    }

    renderTranscript();
}

function resetTranscript() {
    partialText = '';
    lastNonEmptyText = '';
    currentAudioLevel = 0;
    smoothedAudioLevel = 0;
    renderTranscript();
}

function startWaveformLoop() {
    if (waveformFrameId) return;

    const tick = () => {
        waveformFrameId = 0;
        const target = isCompactBatchMode() ? currentAudioLevel : 0;
        const easing = target > smoothedAudioLevel ? WAVEFORM_ATTACK : WAVEFORM_RELEASE;
        smoothedAudioLevel += (target - smoothedAudioLevel) * easing;

        waveformBarNodes.forEach((bar, index) => {
            const weight = WAVEFORM_WEIGHTS[index] ?? 0.5;
            const organic = Math.sin((performance.now() * 0.012) + index * 1.3) * WAVEFORM_JITTER;
            const normalized = clamp((smoothedAudioLevel * weight) + (smoothedAudioLevel * organic), 0, 1);
            const base = 0.12 + weight * 0.12;
            const scale = clamp(base + normalized * 0.92, 0.14, 0.98);
            const opacity = clamp(0.34 + normalized * 0.66, 0.34, 1);
            bar.style.setProperty('--bar-scale', scale.toFixed(3));
            bar.style.setProperty('--bar-opacity', opacity.toFixed(3));
        });

        if (isCompactBatchMode() || smoothedAudioLevel > 0.01) {
            waveformFrameId = window.requestAnimationFrame(tick);
        }
    };

    waveformFrameId = window.requestAnimationFrame(tick);
}

function handleAudioLevelUpdate(level: number | undefined) {
    const rawLevel = clamp(level ?? 0, 0, 1);
    currentAudioLevel = clamp(Math.sqrt(rawLevel) * WAVEFORM_GAIN, 0, 1);
    startWaveformLoop();
}

function renderTranscript() {
    const rawText = partialText.trim() || lastNonEmptyText.trim();
    const batchWaveMode = isBatchWaveMode();
    const compactBatchMode = isCompactBatchMode();

    setBatchLayoutMode(batchWaveMode, compactBatchMode);

    if (compactBatchMode) {
        startWaveformLoop();
    }

    if (batchWaveMode) {
        return;
    }

    if (compactBatchMode) {
        return;
    }

    let display = fitText(rawText);

    if (!display) {
        textArea?.classList.add('is-placeholder');
        const placeholder =
            currentMode === 'correcting'
                ? t('processingText')
                : currentMode === 'recognizing'
                  ? t('recognizing')
                  : currentMode === 'push_to_talk' || currentMode === 'hands_free'
                    ? t('recording')
                    : t('startRecording');
        if (textArea) {
            textArea.innerHTML = `<span class="placeholder">${placeholder}</span>`;
        }
    } else {
        textArea?.classList.remove('is-placeholder');
        if (textArea) {
            textArea.textContent = display;
        }
    }
}

function handleTranscriptUpdate(text: string | undefined, _isFinal: boolean) {
    const trimmed = (text || '').trim();

    if (isCompactBatchMode()) {
        partialText = '';
        lastNonEmptyText = '';
        renderTranscript();
        return;
    }

    if (!trimmed && partialText) {
        renderTranscript();
        return;
    }

    if (currentMode === 'correcting' && !trimmed) {
        renderTranscript();
        return;
    }

    partialText = trimmed;
    if (trimmed) {
        lastNonEmptyText = trimmed;
    }
    renderTranscript();
}

function updateCountdown(seconds?: number | null) {
    if (!countdown) return;
    if (seconds === null || seconds === undefined) {
        countdown.textContent = '';
    } else {
        const minutes = Math.floor(seconds / 60);
        const secs = seconds % 60;
        countdown.textContent = `${minutes}:${secs.toString().padStart(2, '0')}`;
    }
}

function updateIntent(intent?: string) {
    currentIntent = intent === 'translate_en' ? 'translate_en' : 'assistant';
    if (!intentChip) return;

    if (currentIntent === 'translate_en') {
        intentChip.textContent = t('translateEn');
        intentChip.classList.add('translate');
    } else {
        intentChip.textContent = t('assistant');
        intentChip.classList.remove('translate');
    }
}

showIcon('mic');
updateTextAreaMaxWidth();
renderTranscript();
updateIntent('assistant');
handleAudioLevelUpdate(0);

async function initListeners() {
    const unsubs: Array<() => void> = [];
    const add = async <T>(name: string, handler: (event: { payload: T }) => void) => {
        const unsub = await listen<T>(name, handler);
        unsubs.push(unsub);
    };

    await add('state:recording_style', (event: { payload?: { style?: string; batch?: boolean } }) => {
        const style = event.payload?.style;
        const wasIdle = currentMode === 'idle';

        if (style === 'push_to_talk') {
            updateStatus('push_to_talk');
        } else if (style === 'hands_free') {
            updateStatus('hands_free');
        } else {
            if (!isCompactBatchMode()) {
                updateStatus('idle');
            } else {
                renderTranscript();
            }
        }

        if (wasIdle && (style === 'push_to_talk' || style === 'hands_free')) {
            resetTranscript();
        }
    });

    await add('state:recognizing', (event: { payload?: { is_recognizing?: boolean } }) => {
        if (event.payload?.is_recognizing) {
            updateStatus('recognizing');
        } else if (currentMode === 'recognizing') {
            updateStatus('idle');
        }
    });

    await add('state:correcting', (event: { payload?: { is_correcting?: boolean } }) => {
        if (event.payload?.is_correcting) {
            updateStatus('correcting');
        } else if (currentMode === 'correcting') {
            updateStatus('idle');
        }
    });

    await add('state:intent', (event: { payload?: { intent?: string } }) => {
        updateIntent(event.payload?.intent);
    });

    await add('asr:event', (event: { payload?: { text?: string; isFinal?: boolean } }) => {
        const { text, isFinal } = event.payload || {};
        handleTranscriptUpdate(text, !!isFinal);
    });

    await add('state:countdown', (event: { payload?: { seconds?: number } }) => {
        updateCountdown(event.payload?.seconds);
    });

    await add('state:audio_level', (event: { payload?: { level?: number } }) => {
        handleAudioLevelUpdate(event.payload?.level);
    });

    await add('recognition:stopped', () => {
        if (!isCompactBatchMode()) {
            updateStatus('idle');
        } else {
            renderTranscript();
        }
    });

    await add('state:hud_presentation', (event: { payload?: { mode?: string } }) => {
        hudPresentation = event.payload?.mode === 'batch' ? 'batch' : 'stream';
        if (!isCompactBatchMode()) {
            currentAudioLevel = 0;
            smoothedAudioLevel = 0;
        }
        renderTranscript();
    });

    await add<{ locale?: 'zh-CN' | 'en-US' }>('ui:locale-changed', (event) => {
        setHudLocale(event.payload?.locale);
    });

    window.addEventListener('beforeunload', () => {
        if (waveformFrameId) {
            window.cancelAnimationFrame(waveformFrameId);
        }
        unsubs.forEach((fn) => fn && fn());
    });
}

invoke<'zh-CN' | 'en-US'>('get_resolved_ui_locale')
    .then((locale: 'zh-CN' | 'en-US') => {
        setHudLocale(locale);
    })
    .catch((err: unknown) => {
        console.error('[HUD] Failed to resolve locale:', err);
        setHudLocale('en-US');
    })
    .finally(() => {
        initListeners().catch((err) => {
            console.error('[HUD] Failed to initialize:', err);
        });
    });
