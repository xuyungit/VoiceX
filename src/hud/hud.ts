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

// Keep the visible snippet within ~2 lines to avoid CSS ellipsis cutting the tail.
const MAX_DISPLAY_CHARS = 36;

const WAVEFORM_BARS_HTML = '<div class="waveform-bars"><span></span><span></span><span></span><span></span><span></span></div>';

function renderTranscript() {
    let display = partialText.trim() || lastNonEmptyText.trim();
    if (display.length > MAX_DISPLAY_CHARS) {
        display = '...' + display.slice(-MAX_DISPLAY_CHARS);
    }

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
