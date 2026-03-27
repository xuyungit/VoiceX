// VoiceX HUD Logic (TypeScript source)
import { listen } from '@tauri-apps/api/event';

const statusIcon = document.getElementById('statusIcon');
const countdown = document.getElementById('countdown');
const textArea = document.getElementById('textArea');
const intentChip = document.getElementById('intentChip');

if (!statusIcon || !countdown || !textArea || !intentChip) {
    console.error('[HUD] Missing required DOM elements', {
        statusIcon: !!statusIcon,
        countdown: !!countdown,
        textArea: !!textArea,
        intentChip: !!intentChip
    });
}

let currentMode: 'idle' | 'push_to_talk' | 'hands_free' | 'correcting' = 'idle';
let currentIntent: 'assistant' | 'translate_en' = 'assistant';
let lastActiveIcon: 'mic' | 'waveform' | 'wand' = 'mic';
let partialText = '';
let lastNonEmptyText = '';

const icons: Record<string, Element | null | undefined> = {
    mic: statusIcon?.querySelector('.icon-mic') ?? null,
    waveform: statusIcon?.querySelector('.icon-waveform') ?? null,
    wand: statusIcon?.querySelector('.icon-wand') ?? null
};

function showIcon(name: keyof typeof icons) {
    Object.values(icons).forEach((icon) => icon?.classList.remove('active'));
    icons[name]?.classList.add('active');
}

function updateStatus(mode: typeof currentMode) {
    currentMode = mode;
    document.body.classList.remove('recording', 'correcting');

    switch (mode) {
        case 'push_to_talk':
            document.body.classList.add('recording');
            showIcon('mic');
            statusIcon?.classList.add('animating');
            lastActiveIcon = 'mic';
            break;
        case 'hands_free':
            document.body.classList.add('recording');
            showIcon('waveform');
            statusIcon?.classList.add('animating');
            lastActiveIcon = 'waveform';
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
}

function resetTranscript() {
    partialText = '';
    lastNonEmptyText = '';
    renderTranscript();
}

// Keep the visible snippet within ~2 lines to avoid CSS ellipsis cutting the tail.
const MAX_DISPLAY_CHARS = 36;

function renderTranscript() {
    let display = partialText.trim() || lastNonEmptyText.trim();
    if (display.length > MAX_DISPLAY_CHARS) {
        display = '...' + display.slice(-MAX_DISPLAY_CHARS);
    }

    if (!display) {
        textArea?.classList.add('is-placeholder');
        const placeholder =
            currentMode === 'correcting'
                ? '正在处理文本...'
                : currentMode === 'push_to_talk' || currentMode === 'hands_free'
                  ? '正在识别...'
                  : '按下热键开始录音';
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
        intentChip.textContent = 'EN Translate';
        intentChip.classList.add('translate');
    } else {
        intentChip.textContent = 'Assistant';
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

    await add('state:recording_style', (event: { payload?: { style?: string } }) => {
        const style = event.payload?.style;
        const wasIdle = currentMode === 'idle';

        if (style === 'push_to_talk') {
            updateStatus('push_to_talk');
        } else if (style === 'hands_free') {
            updateStatus('hands_free');
        } else {
            updateStatus('idle');
        }

        if (wasIdle && (style === 'push_to_talk' || style === 'hands_free')) {
            resetTranscript();
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

    window.addEventListener('beforeunload', () => {
        unsubs.forEach((fn) => fn && fn());
    });
}

initListeners().catch((err) => {
    console.error('[HUD] Failed to initialize:', err);
});
