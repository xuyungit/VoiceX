// VoiceX HUD Logic (TypeScript source)
import { invoke } from '@tauri-apps/api/core';
import { listen } from '@tauri-apps/api/event';
import zhCN from '../i18n/locales/zh-CN';
import enUS from '../i18n/locales/en-US';

const statusIcon = document.getElementById('statusIcon');
const countdown = document.getElementById('countdown');
const textArea = document.getElementById('textArea');
const intentChip = document.getElementById('intentChip');
const titleElement = document.querySelector('title');

if (!statusIcon || !countdown || !textArea || !intentChip) {
    console.error('[HUD] Missing required DOM elements', {
        statusIcon: !!statusIcon,
        countdown: !!countdown,
        textArea: !!textArea,
        intentChip: !!intentChip
    });
}

let currentMode: 'idle' | 'push_to_talk' | 'hands_free' | 'recognizing' | 'correcting' = 'idle';
let currentIntent: 'assistant' | 'translate_en' = 'assistant';
let isBatchRecording = false;
let lastActiveIcon: 'mic' | 'waveform' | 'cloud' | 'wand' = 'mic';
let partialText = '';
let lastNonEmptyText = '';
let currentLocale: 'zh-CN' | 'en-US' = 'en-US';

// --- Pixel-based text measurement via OffscreenCanvas ---
// Instead of a fixed character limit (MAX_DISPLAY_CHARS), we measure actual pixel
// widths so the same logic works for CJK, Latin, and mixed text automatically.
const HUD_FONT = '12px -apple-system, BlinkMacSystemFont, "Segoe UI", "PingFang SC", "Hiragino Sans GB", "Microsoft YaHei", sans-serif';
const MAX_LINES = 2;
const ELLIPSIS = '\u2026';            // "…"

let measureCtx: OffscreenCanvasRenderingContext2D | CanvasRenderingContext2D | null = null;
let textAreaMaxWidth = 0;             // available pixel width (updated on resize)

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
  return ctx ? ctx.measureText(text).width : text.length * 7; // fallback estimate
}

/** Compute the available text width from the live DOM layout (accounts for padding, border, DPI). */
function updateTextAreaMaxWidth() {
  if (!textArea) return;
  // Use clientWidth which is the inner width minus scrollbar, inside padding
  const style = getComputedStyle(textArea);
  const pl = parseFloat(style.paddingLeft) || 0;
  const pr = parseFloat(style.paddingRight) || 0;
  textAreaMaxWidth = textArea.clientWidth - pl - pr;
  if (textAreaMaxWidth <= 0) textAreaMaxWidth = 230; // safe fallback
}

/**
 * Truncate `text` from the left so that "…" + tail fits within MAX_LINES lines
 * at the current textAreaMaxWidth.  Returns the display string.
 */
function fitText(text: string): string {
  if (!text) return text;
  const maxW = textAreaMaxWidth || 230;
  // Subtract a small margin per line to account for browser line-breaking overhead
  // (word boundaries leave unused space at line ends).
  const totalBudget = (maxW - 4) * MAX_LINES;

  // Fast path: text already fits
  if (measureText(text) <= totalBudget) return text;

  const ellipsisW = measureText(ELLIPSIS);
  const budget = totalBudget - ellipsisW;

  // Binary search for the longest tail that fits
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

// Recompute available width when the window resizes (e.g. DPI change).
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

function updateStatus(mode: typeof currentMode) {
    currentMode = mode;
    document.body.classList.remove('recording', 'recognizing', 'correcting');

    switch (mode) {
        case 'push_to_talk':
            document.body.classList.add('recording');
            showIcon(isBatchRecording ? 'waveform' : 'mic');
            statusIcon?.classList.add('animating');
            lastActiveIcon = isBatchRecording ? 'waveform' : 'mic';
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
    renderTranscript();
}

const WAVEFORM_BARS_HTML = '<div class="waveform-bars"><span></span><span></span><span></span><span></span><span></span></div>';

function renderTranscript() {
    let display = partialText.trim() || lastNonEmptyText.trim();
    display = fitText(display);

    if (!display) {
        textArea?.classList.add('is-placeholder');

        // Batch recording: show animated waveform bars instead of text placeholder.
        if (isBatchRecording && (currentMode === 'push_to_talk' || currentMode === 'hands_free')) {
            textArea!.innerHTML = WAVEFORM_BARS_HTML;
            return;
        }

        const placeholder =
            currentMode === 'correcting'
                ? t('processingText')
                : currentMode === 'recognizing'
                  ? t('recognizing')
                  : currentMode === 'push_to_talk' || currentMode === 'hands_free'
                    ? t('recording')
                    : t('startRecording');
        textArea!.innerHTML = `<span class="placeholder">${placeholder}</span>`;
    } else {
        textArea?.classList.remove('is-placeholder');
        textArea!.textContent = display;
    }
}

function handleTranscriptUpdate(text: string | undefined, _isFinal: boolean) {
    const trimmed = (text || '').trim();

    // If we already have content and the update is empty, keep showing the last text
    // (prevents placeholder flashes while correcting/finalizing).
    if (!trimmed && partialText) {
        renderTranscript();
        return;
    }

    // During correcting, also ignore empty updates.
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
    if (seconds === null || seconds === undefined) {
        countdown!.textContent = '';
    } else {
        const minutes = Math.floor(seconds / 60);
        const secs = seconds % 60;
        countdown!.textContent = `${minutes}:${secs.toString().padStart(2, '0')}`;
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

async function initListeners() {
    const unsubs: Array<() => void> = [];
    const add = async <T>(name: string, handler: (event: { payload: T }) => void) => {
        const unsub = await listen<T>(name, handler);
        unsubs.push(unsub);
    };

    await add('state:recording_style', (event: { payload?: { style?: string; batch?: boolean } }) => {
        const style = event.payload?.style;
        const wasIdle = currentMode === 'idle';
        isBatchRecording = !!event.payload?.batch;

        if (style === 'push_to_talk') {
            updateStatus('push_to_talk');
        } else if (style === 'hands_free') {
            updateStatus('hands_free');
        } else {
            isBatchRecording = false;
            updateStatus('idle');
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

    await add('recognition:stopped', () => {
        updateStatus('idle');
    });

    await add<{ locale?: 'zh-CN' | 'en-US' }>('ui:locale-changed', (event) => {
        setHudLocale(event.payload?.locale);
    });

    window.addEventListener('beforeunload', () => {
        unsubs.forEach((fn) => fn && fn());
    });
}

invoke<'zh-CN' | 'en-US'>('get_resolved_ui_locale')
    .then((locale) => {
        setHudLocale(locale);
    })
    .catch((err) => {
        console.error('[HUD] Failed to resolve locale:', err);
        setHudLocale('en-US');
    })
    .finally(() => {
        initListeners().catch((err) => {
            console.error('[HUD] Failed to initialize:', err);
        });
    });
