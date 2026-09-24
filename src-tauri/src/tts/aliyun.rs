//! Alibaba Cloud Model Studio (百炼) speech synthesis backend.
//!
//! Covers four model families behind one provider. Qwen3-TTS is its own
//! service. Qwen-Audio-3.0-TTS and CosyVoice-v3 share the SpeechSynthesizer
//! endpoint and the same request spelling — CosyVoice is the engine behind
//! both, which is why their error messages say `[cosyvoice:]` — but the voice
//! ids are not interchangeable. CosyVoice-v3.5-flash uses that same path and
//! spelling, with no system preset voices: every request has to name a cloned
//! or designed `voice_id` bound to this model, or the engine returns 418.
//! Over HTTP with server-sent events they all hand back the same thing in the
//! same shape: base64 MP3 in `output.audio.data`, chunk after chunk, with a
//! URL in the final frame that we discard. That similarity is what makes one
//! backend reasonable; [`ModelSpec`] holds everything that is genuinely per
//! model.
//!
//! Chosen over the two WebSocket protocols on the same measurement that settled
//! the Volcengine backend: the text is fully known before the request goes out,
//! so incremental input buys nothing, and first audio arrives in 407-539 ms
//! either way. See `docs/aliyun-tts-provider-research-2026-08-13.md` for the
//! probe results, including the several documented parameters that do not exist
//! and the several undocumented ones that do.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Sender};
use std::sync::{Arc, Mutex};
use std::thread;

use base64::Engine;
use futures_util::StreamExt;
use serde_json::{json, Value};

use super::caption_timeline::{self, CaptionTimeline};
use super::cloud_playback::{self, PieceStream};
use super::playback::{negotiate_sample_rate_among, prebuffer_samples, PlaybackHandle};
use super::{
    cloud_http_client, log_cloud_retry, log_event, piece_limit_for, split_for_backend, CancelToken,
    CloudStreamError, SpeechProgress, TtsBackend, TtsError, TtsRequest, TtsStatus, TtsVoice,
};

/// Region host. The workspace-scoped `{id}.cn-beijing.maas.aliyuncs.com` form
/// is what the documentation now advertises, but the plain host still serves
/// both models over both HTTP and WebSocket, so requiring a workspace id would
/// be one more mandatory field for no gain. If the plain host is ever retired,
/// `crate::asr::funasr_client::qwen_workspace_host` already builds the other.
const HOST: &str = "https://dashscope.aliyuncs.com";

pub const MODEL_QWEN3: &str = "qwen3-tts-flash";
pub const MODEL_QWEN_AUDIO: &str = "qwen-audio-3.0-tts-flash";
pub const MODEL_QWEN_AUDIO_31: &str = "qwen-audio-3.1-tts-flash";
pub const MODEL_COSYVOICE: &str = "cosyvoice-v3-flash";
pub const MODEL_COSYVOICE_V35: &str = "cosyvoice-v3.5-flash";

/// The style instruction a fresh install reads with — and an upgraded one,
/// since the setting is new and a missing key loads the default. Technical
/// text is what the reader is for, and every voice's own delivery is livelier
/// than that wants. The service also restarts its prosody every ~120 characters (it
/// synthesizes a request in sentence-packed chunks with no context between
/// them), and this wording made those seams the least audible of those tried:
/// the timbre jump (MFCC distance) across a chunk cut fell from 29.1 to 24.8,
/// against 21–22 between sentences inside a chunk (qwen-audio-3.1-tts-flash,
/// 2026-09-24). "标准播音风格" was tried and does the opposite — broadcast
/// delivery is itself expressive.
pub const DEFAULT_INSTRUCTION: &str = "语调平和，语速均匀，音量稳定，客观陈述，不夸张不起伏";

pub fn default_model() -> &'static str {
    // Qwen-Audio 3.1 over 3.0: same endpoint, same 9000-character piece size,
    // and about a sixth of the cost for the same text (billed on audio tokens,
    // 12.5 per second, instead of per input character — measured 2026-09-23,
    // docs/qwen-audio-3.1-research-2026-09-23.md). 3.0 keeps the ~600 "basic"
    // voices 3.1 does not have; Qwen3 remains the pick for its dialect voices.
    // Keep the frontend store default (settings.ts) in step with this.
    MODEL_QWEN_AUDIO_31
}

/// Everything that differs between the model families.
///
/// The parameter names look like gratuitous variation — `speech_rate` against
/// `rate`, `response_format` against `format` — but they are separate services
/// and each ignores the other's spelling silently, returning 200 and audio at
/// the default setting. That silence is why these are a table rather than an
/// attempt to send both spellings and let the server sort it out.
struct ModelSpec {
    id: &'static str,
    path: &'static str,
    /// Rates the model renders, best first. Narrower than the playback module's
    /// own list for `qwen3-tts-flash`, which has no 44.1 or 22.05 kHz.
    sample_rates: &'static [u32],
    /// Longest text accepted in one request.
    max_chars: usize,
    /// Longest text one request reliably *finishes*. Distinct from
    /// `max_chars`, which is where the service starts rejecting: CosyVoice
    /// accepts thousands of characters and then silently truncates the audio
    /// at ~836 output tokens (~33 s at 1x) per internal batch, reporting
    /// `finish_reason=stop` as if nothing happened. Batches are sentences
    /// merged server-side to ~150 characters, so the only reliable control is
    /// keeping the whole request under the budget. Measured 2026-08: a
    /// 163-character single-sentence request lost its tail on every rate and
    /// sample rate; 86 characters always completed; replacing clause commas
    /// with full stops did not help because the server merges the sentences
    /// straight back into one batch.
    piece_chars: usize,
    /// Whether `input.instruction` is sent. Qwen-Audio takes free text with
    /// any voice; CosyVoice v3.5 takes it with the designed/cloned voices that
    /// are its only kind. CosyVoice v3's system voices accept only the fixed
    /// phrasings on the voice list and answer anything else with `Engine
    /// return error code: 428`, and Qwen3-TTS has no such field on this model.
    accepts_instruction: bool,
    /// Whether the stream names each chunk it synthesizes and, asked with
    /// `word_timestamp_enabled`, when every character of it is spoken. With
    /// captions on, such a model keeps the read in one request and each
    /// caption is placed by that timing (see [`caption_timeline`]); every
    /// other model goes out in caption-sized requests, one caption each.
    /// Verified on Qwen-Audio 3.1 (2026-09-24): chunk texts concatenate back
    /// to the input exactly, chunk starts land within 0.2 s of the audio, and
    /// the timestamps leave the audio byte-for-byte unchanged.
    timed_chunks: bool,
    voices: VoiceSource,
    /// Always called with this spec's own `id`, so the body cannot name a
    /// different model than the spec it belongs to — a copy-pasted entry that
    /// forgot to re-thread the constant used to compile fine and send requests
    /// for the wrong model.
    build_body: fn(model: &'static str, s: &Synthesis) -> Value,
}

/// Where a model's voice ids come from.
enum VoiceSource {
    /// System presets the picker can list; the first is the default.
    Preset(&'static [(&'static str, &'static str, &'static str)]),
    /// No presets at all: every request has to name a cloned or designed
    /// voice bound to this model, or the engine returns 418. Those ids belong
    /// to the account that created them, so there is nothing to ship as a
    /// default and nothing for the picker to list — the settings page shows a
    /// text field instead.
    Custom,
}

impl ModelSpec {
    fn presets(&self) -> &'static [(&'static str, &'static str, &'static str)] {
        match self.voices {
            VoiceSource::Preset(voices) => voices,
            VoiceSource::Custom => &[],
        }
    }

    fn default_voice(&self) -> Option<&'static str> {
        self.presets().first().map(|(id, _, _)| *id)
    }

    fn custom_voice_only(&self) -> bool {
        matches!(self.voices, VoiceSource::Custom)
    }
}

/// The synthesis parameters, already on the provider's own scales.
struct Synthesis<'a> {
    text: &'a str,
    voice: &'a str,
    sample_rate: u32,
    /// Speed multiplier, 1.0 neutral, as both families take it.
    rate: f32,
    /// Already filtered by `accepts_instruction`; `None` sends no field.
    instruction: Option<&'a str>,
    /// Ask for per-character timing; only a `timed_chunks` model is asked.
    word_timestamps: bool,
}

/// Qwen3-TTS. Voices are English given names; the dialect voices are the
/// reason to reach for this family over the other one.
const QWEN3_VOICES: [(&str, &str, &str); 12] = [
    ("Cherry", "芊悦", "zh-CN"),
    ("Serena", "苏瑶", "zh-CN"),
    ("Ethan", "晨煦", "zh-CN"),
    ("Chelsie", "千雪", "zh-CN"),
    ("Nofish", "不吃鱼", "zh-CN"),
    ("Dylan", "北京-晓东", "zh-CN"),
    ("Jada", "上海-阿珍", "zh-CN"),
    ("Sunny", "四川-晴儿", "zh-CN"),
    ("Rocky", "粤语-阿强", "zh-CN"),
    ("Kiki", "粤语-阿清", "zh-CN"),
    ("Jennifer", "詹妮弗", "en-US"),
    ("Ryan", "甜茶", "en-US"),
];

/// Qwen-Audio-3.0-TTS, which shares its engine with CosyVoice — the error
/// messages say `[cosyvoice:]` outright. Voice ids still do not carry across:
/// `longanhuan_v3.6` here is not `longanhuan` / `longanhuan_v3` on CosyVoice.
///
/// The first twelve are the flash model's complete system-voice list. Below
/// them is a reading-shaped pick from the ~600 "basic" voices the same model
/// serves under `qwen-audio-3.0-tts-flash-<suffix>` ids (the 有声阅读/知识分享
/// rows of the published Excel); the picker's typed-id path reaches the rest.
/// Every entry here returned audio from the live account on 2026-08-30.
const QWEN_AUDIO_VOICES: [(&str, &str, &str); 19] = [
    ("longanfengyue", "龙安风悦", "zh-CN"),
    ("longanyuanfei", "龙安元妃", "zh-CN"),
    ("longanlingxi", "龙安灵希", "zh-CN"),
    ("longanxiaoxin", "龙安小昕", "zh-CN"),
    ("longanhuan_v3.6", "龙安欢", "zh-CN"),
    ("longjielidou_v3.6", "龙杰力豆", "zh-CN"),
    ("longpaopao_v3.6", "龙泡泡", "zh-CN"),
    ("longhuohuo_v3.6", "龙火火", "zh-CN"),
    ("longchuanshu_v3.6", "龙川叔", "zh-CN"),
    ("loongmary", "loongmary", "en-GB"),
    ("loongeva_v3.6", "loongeva", "en-US"),
    ("loongjohn", "loongJohn", "en-US"),
    ("qwen-audio-3.0-tts-flash-longrongtaolian", "龙蓉桃涟（有声阅读）", "zh-CN"),
    ("qwen-audio-3.0-tts-flash-longyimuling", "龙翼暮凌（有声阅读）", "zh-CN"),
    ("qwen-audio-3.0-tts-flash-longyuyaoluan", "龙羽瑶鸾（有声阅读）", "zh-CN"),
    ("qwen-audio-3.0-tts-flash-longnilanfeng", "龙霓岚凤（有声阅读）", "zh-CN"),
    ("qwen-audio-3.0-tts-flash-longjinlianxiao", "龙瑾涟晓（有声阅读）", "zh-CN"),
    ("qwen-audio-3.0-tts-flash-loongadriangao", "Adrian Gao（阅读）", "en-US"),
    ("qwen-audio-3.0-tts-flash-loongivyhu", "Ivy Hu（朗诵）", "en-US"),
];

/// Qwen-Audio-3.1-TTS system voices: every non-character voice on the published
/// list (the 15 role-play voices are reachable through the picker's typed-id
/// path). All carry a `_v3.1` suffix, and 3.0's ids — basic voices included —
/// come back as `Engine error [411]`, so 3.1 needs its own voice setting.
/// There is no 3.1 equivalent of 3.0's ~600 basic voices.
///
/// The default leads: its listed uses are 有声书、旁白、新闻播报, i.e. reading.
/// `longanfengyue_v3.1` is *not* 3.0's default voice with a suffix — on 3.1 it
/// is a 东北话 voice. Every entry here returned audio on 2026-09-23.
const QWEN_AUDIO_31_VOICES: [(&str, &str, &str); 41] = [
    ("anxiaolan_v3.1", "安小岚", "zh-CN"),
    ("xieshurou_v3.1", "谢舒柔", "zh-CN"),
    ("wenhuaizhi_v3.1", "闻怀之", "zh-CN"),
    ("xuyanchu_v3.1", "许言初", "zh-CN"),
    ("xuyuyuan_v3.1", "许玉远", "zh-CN"),
    ("xiaoxingzhi_v3.1", "萧行之", "zh-CN"),
    ("guyunshu_v3.1", "顾云舒", "zh-CN"),
    ("yeqinghe_v3.1", "叶清禾", "zh-CN"),
    ("wenhuaiqing_v3.1", "温怀清", "zh-CN"),
    ("yuxiaoyun_v3.1", "于小云", "zh-CN"),
    ("qiaoxiaojiao_v3.1", "乔小娇", "zh-CN"),
    ("xiaxiaochen_v3.1", "夏小晨", "zh-CN"),
    ("baiqinglan_v3.1", "白清岚", "zh-CN"),
    ("anruorou_v3.1", "安若柔", "zh-CN"),
    ("yunhuanhuan_v3.1", "云欢欢", "zh-CN"),
    ("xuxiaoqiao_v3.1", "徐小俏", "zh-CN"),
    ("baianran_v3.1", "白安然", "zh-CN"),
    ("yezhiqing_v3.1", "叶知晴", "zh-CN"),
    ("anyuqing_v3.1", "安语晴", "zh-CN"),
    ("anmingyuan_v3.1", "安明远", "zh-CN"),
    ("huozhuoshi_v3.1", "霍拙石", "zh-CN"),
    ("andi_v3.1", "安迪（ABC 口音）", "zh-CN"),
    ("longanhuan_v3.1", "龙安欢（重庆话）", "zh-CN"),
    ("longanlingxin_v3.1", "龙安灵心（云南话）", "zh-CN"),
    ("longanfengyue_v3.1", "龙安风悦（东北话）", "zh-CN"),
    ("xunanchuan_v3.1", "许南川（甘肃话）", "zh-CN"),
    ("Emily_v3.1", "Emily", "en-GB"),
    ("Luna_v3.1", "Luna", "en-GB"),
    ("Eric_v3.1", "Eric", "en-GB"),
    ("Luca_v3.1", "Luca", "en-GB"),
    ("Abby_v3.1", "Abby", "en-US"),
    ("Annie_v3.1", "Annie", "en-US"),
    ("Ava_v3.1", "Ava", "en-US"),
    ("Beth_v3.1", "Beth", "en-US"),
    ("Betty_v3.1", "Betty", "en-US"),
    ("Cally_v3.1", "Cally", "en-US"),
    ("Cindy_v3.1", "Cindy", "en-US"),
    ("Donna_v3.1", "Donna", "en-US"),
    ("Andy_v3.1", "Andy", "en-US"),
    ("Brian_v3.1", "Brian", "en-US"),
    ("David_v3.1", "David", "en-US"),
];

/// CosyVoice-v3-flash system voices. The service has eighty-plus; this is the
/// reading-shaped subset that the live account accepted. The picker still
/// accepts a typed id, so the rest stay reachable.
///
/// Language tags follow the voice list's primary language and the convention
/// the Qwen3 table set: dialect voices (Cantonese included) are `zh-CN`, so
/// filtering the picker by language behaves the same under every model. Per
/// the published list, `loongbella_v3` is a Mandarin+English voice despite the
/// `loong` prefix that marks the English-only ones.
const COSYVOICE_VOICES: [(&str, &str, &str); 10] = [
    ("longanyang", "龙安洋", "zh-CN"),
    ("longanhuan", "龙安欢", "zh-CN"),
    ("longanhuan_v3", "龙安欢（方言）", "zh-CN"),
    ("longhuhu_v3", "龙呼呼", "zh-CN"),
    ("longjiaxin_v3", "龙嘉欣（粤语）", "zh-CN"),
    ("longlaotie_v3", "龙老铁", "zh-CN"),
    ("longsanshu_v3", "龙三叔", "zh-CN"),
    ("longshuo_v3", "龙硕", "zh-CN"),
    ("loongabby_v3", "loongabby", "en-US"),
    ("loongbella_v3", "Bella3.0", "zh-CN"),
];

fn speech_synthesizer_body(model: &'static str, s: &Synthesis<'_>) -> Value {
    let mut body = json!({
        "model": model,
        "input": {
            "text": s.text,
            "voice": s.voice,
            "format": "mp3",
            "sample_rate": s.sample_rate,
            "rate": s.rate,
        },
    });
    if let Some(instruction) = s.instruction {
        body["input"]["instruction"] = json!(instruction);
    }
    if s.word_timestamps {
        body["input"]["word_timestamp_enabled"] = json!(true);
    }
    body
}

const SPECS: [ModelSpec; 5] = [
    ModelSpec {
        id: MODEL_QWEN3,
        path: "/api/v1/services/aigc/multimodal-generation/generation",
        sample_rates: &[48_000, 24_000, 16_000],
        // Measured: 5000 characters are accepted, and the documented 512-token
        // ceiling does not exist. First audio does grow with length — 479 ms at
        // 500 characters, 1.9 s at 5000 — so this is the limit, not a target.
        max_chars: 5_000,
        // Renders ~4x realtime and completed every long-text probe intact.
        piece_chars: 5_000,
        accepts_instruction: false,
        timed_chunks: false,
        voices: VoiceSource::Preset(&QWEN3_VOICES),
        build_body: |model, s| {
            json!({
                "model": model,
                "input": {
                    "text": s.text,
                    "voice": s.voice,
                    // Selections are routinely mixed Chinese and English, and
                    // naming one language makes the other one read badly.
                    "language_type": "Auto",
                },
                "parameters": {
                    "response_format": "mp3",
                    "sample_rate": s.sample_rate,
                    "speech_rate": s.rate,
                },
            })
        },
    },
    ModelSpec {
        id: MODEL_QWEN_AUDIO,
        path: "/api/v1/services/audio/tts/SpeechSynthesizer",
        sample_rates: &[48_000, 44_100, 24_000, 22_050, 16_000],
        // The service caps a request at 20000 of its own units and counts a
        // CJK character as two of them: 20000 characters were reported back
        // as 36000 and 15000 as 27000, both rejected; 10000 were accepted.
        // 9000 keeps a pure-CJK selection at 18000 units, short of the edge.
        max_chars: 9_000,
        // Shares CosyVoice's engine but not its budget: the 163-character
        // single-sentence probe came back complete, ~10x realtime.
        piece_chars: 9_000,
        accepts_instruction: true,
        timed_chunks: false,
        voices: VoiceSource::Preset(&QWEN_AUDIO_VOICES),
        build_body: speech_synthesizer_body,
    },
    ModelSpec {
        id: MODEL_QWEN_AUDIO_31,
        path: "/api/v1/services/audio/tts/SpeechSynthesizer",
        // Measured 2026-09-23: every rate here comes back in the MP3 frame
        // header as requested.
        sample_rates: &[48_000, 44_100, 24_000, 22_050, 16_000],
        // Same 20000-unit cap and CJK-counts-double rule as 3.0: 10000
        // characters accepted, 15000 rejected as "limited: 20000, current:
        // 27000".
        max_chars: 9_000,
        // No output budget either: a 3000-character mixed prose/digit piece
        // came back as 590 s of audio at the same 5.1 characters/s as a
        // 163-character one, rendered at ~12x realtime.
        piece_chars: 9_000,
        accepts_instruction: true,
        timed_chunks: true,
        voices: VoiceSource::Preset(&QWEN_AUDIO_31_VOICES),
        build_body: speech_synthesizer_body,
    },
    ModelSpec {
        id: MODEL_COSYVOICE,
        path: "/api/v1/services/audio/tts/SpeechSynthesizer",
        // Measured: every rate on this list comes back honoured in the MP3
        // frame header, so the device-preferred rate the playback module
        // negotiates is safe to request.
        sample_rates: &[48_000, 44_100, 24_000, 22_050, 16_000],
        // Same 20000-unit cap as qwen-audio, with the same CJK-counts-double
        // rule: 15000 characters were rejected as 27000 units, 10000 accepted.
        max_chars: 9_000,
        // The ~836-token batch budget (see `piece_chars` on the struct) bites
        // at ~155 characters of dense prose, sooner for text that speaks
        // slower — digits and heavy punctuation expand. 120 keeps ordinary
        // text a comfortable margin inside it while cutting only once per
        // ~half minute of speech.
        piece_chars: 120,
        accepts_instruction: false,
        timed_chunks: false,
        voices: VoiceSource::Preset(&COSYVOICE_VOICES),
        build_body: speech_synthesizer_body,
    },
    ModelSpec {
        id: MODEL_COSYVOICE_V35,
        path: "/api/v1/services/audio/tts/SpeechSynthesizer",
        sample_rates: &[48_000, 44_100, 24_000, 22_050, 16_000],
        max_chars: 9_000,
        // Same silent-truncation class as v3-flash, measured 2026-09-08 with
        // a designed voice: a period-free 180-character request returned
        // complete audio (~5.2 kB/char); 200 characters returned an ID3
        // header and nothing else, `finish_reason=stop`, billed in full.
        // That single plain-prose point says nothing about digit-heavy or
        // slow-rate text, which expands on this engine exactly as on v3, so
        // v3's 120 applies unchanged until dense text is measured here too.
        piece_chars: 120,
        accepts_instruction: true,
        timed_chunks: false,
        voices: VoiceSource::Custom,
        build_body: speech_synthesizer_body,
    },
];

fn spec_for(model: &str) -> &'static ModelSpec {
    SPECS
        .iter()
        .find(|spec| spec.id == model)
        .unwrap_or(&SPECS[0])
}

pub fn default_voice_for(model: &str) -> &'static str {
    spec_for(model).default_voice().unwrap_or("")
}

#[derive(Debug, Clone)]
pub struct AliyunConfig {
    pub api_key: String,
    pub model: String,
    /// Natural-language style instruction; empty sends none. Dropped for
    /// models that reject it rather than failing the read.
    pub instruction: String,
}

/// Convert the stored 0.0..=1.0 rate into the provider's own multiplier.
///
/// The stored value is normalized around the macOS engine default of 0.5, shown
/// as 1x on a 0.5x-2x slider, and every model family takes exactly that
/// multiplier over exactly that range — so unlike Volcengine's -50..=100 this
/// needs no remapping, only the same clamp.
fn speed_from_normalized(rate: Option<f32>) -> f32 {
    let Some(rate) = rate else { return 1.0 };
    (rate / 0.5).clamp(0.5, 2.0)
}

/// Drop the space between Chinese and a token that contains a digit — the
/// typographic spacing the cleanup and translate prompts' LLMs put around
/// every number — before the text reaches the service.
///
/// The service reads that space as a pause: Qwen-Audio 3.0 and 3.1 normalize
/// 「每种配置有 8 个」 to 「有、八、个」. Over 15 sentences from translate-read
/// history, closing these spaces took 3.1's inserted 、 from 18 to 1 and
/// changed no reading otherwise; CosyVoice v3, which reports no normalized
/// text, spoke four test sentences 13% faster (2026-09-24). Letter-only words
/// are left alone: their spaces add no 、, and closing them moved the audio no
/// more than resending the same text an hour later does. Captions keep the
/// spaced text; only what is synthesized changes.
///
/// Returns the indices of the characters kept, in order.
fn number_spacing_kept(chars: &[char]) -> Vec<usize> {
    fn is_cjk(ch: char) -> bool {
        matches!(ch,
            '\u{3000}'..='\u{303f}'    // CJK punctuation: 。、「」
            | '\u{3400}'..='\u{4dbf}'  // extension A
            | '\u{4e00}'..='\u{9fff}'  // unified ideographs
            | '\u{f900}'..='\u{faff}'  // compatibility ideographs
            | '\u{ff00}'..='\u{ffef}'  // full-width forms: ，：（）
        )
    }
    // The run of non-CJK characters on one side of the space, up to the next
    // whitespace: `0.94%` in 「为 0.94%，」, `M3` in 「从 M3 版本」.
    fn has_digit<'a>(token: impl Iterator<Item = &'a char>) -> bool {
        token
            .take_while(|ch| !ch.is_whitespace() && !is_cjk(**ch))
            .any(|ch| ch.is_ascii_digit())
    }

    (0..chars.len())
        .filter(|&index| {
            let (Some(&before), Some(&after)) = (
                index.checked_sub(1).and_then(|i| chars.get(i)),
                chars.get(index + 1),
            ) else {
                return true;
            };
            let closes = chars[index] == ' '
                && ((is_cjk(before) && has_digit(chars[index + 1..].iter()))
                    || (is_cjk(after) && has_digit(chars[..index].iter().rev())));
            !closes
        })
        .collect()
}

pub struct AliyunBackend {
    config: Mutex<AliyunConfig>,
    /// Filled in by the decode thread once it owns the output device, so `stop`
    /// can cut the audio immediately instead of waiting for the network side to
    /// notice. Shared rather than copied: the handle does not exist yet when
    /// `start` returns.
    playback: Arc<Mutex<Option<PlaybackHandle>>>,
    speaking: Arc<AtomicBool>,
    /// The current request's text as the pieces it was synthesized in,
    /// which is what `progress` reports on.
    pieces: Mutex<Vec<String>>,
    /// Set instead for a read whose captions the service times; `progress`
    /// then reports on this. Each read gets its own, so a superseded read
    /// still finishing a frame cannot place captions in the next one.
    timeline: Mutex<Option<Arc<Mutex<CaptionTimeline>>>>,
}

impl AliyunBackend {
    pub fn new(config: AliyunConfig) -> Self {
        Self {
            config: Mutex::new(config),
            playback: Arc::new(Mutex::new(None)),
            speaking: Arc::new(AtomicBool::new(false)),
            pieces: Mutex::new(Vec::new()),
            timeline: Mutex::new(None),
        }
    }

    pub fn apply_config(&self, config: AliyunConfig) {
        if let Ok(mut slot) = self.config.lock() {
            *slot = config;
        }
    }

    fn config(&self) -> Result<AliyunConfig, TtsError> {
        let config = self
            .config
            .lock()
            .map(|slot| slot.clone())
            .map_err(|_| TtsError::Backend("configuration is poisoned".to_string()))?;
        if config.api_key.trim().is_empty() {
            return Err(TtsError::Backend("no API key configured".to_string()));
        }
        Ok(config)
    }

    fn spec(&self) -> &'static ModelSpec {
        self.config
            .lock()
            .map(|slot| spec_for(&slot.model))
            .unwrap_or(&SPECS[0])
    }
}

impl TtsBackend for AliyunBackend {
    fn name(&self) -> &'static str {
        "aliyun"
    }

    fn list_voices(&self) -> Result<Vec<TtsVoice>, TtsError> {
        Ok(self
            .spec()
            .presets()
            .iter()
            .map(|(id, name, language)| TtsVoice {
                id: id.to_string(),
                name: name.to_string(),
                language: language.to_string(),
            })
            .collect())
    }

    fn custom_voice_only(&self) -> bool {
        self.spec().custom_voice_only()
    }

    fn start(&self, request: TtsRequest, token: CancelToken) -> Result<(), TtsError> {
        // Every failure path has to hand the session back, or the session stays
        // claimed: the hotkey is stuck in "stop" mode from then on and the HUD
        // sits on "preparing" forever.
        self.begin(request, token.clone()).inspect_err(|_| {
            token.finish();
        })
    }

    fn stop(&self) -> Result<(), TtsError> {
        self.speaking.store(false, Ordering::SeqCst);
        if let Ok(slot) = self.playback.lock() {
            if let Some(handle) = slot.as_ref() {
                handle.stop();
            }
        }
        Ok(())
    }

    fn status(&self) -> TtsStatus {
        if self.speaking.load(Ordering::SeqCst) {
            TtsStatus::Speaking
        } else {
            TtsStatus::Idle
        }
    }

    fn audio_level(&self) -> Option<f32> {
        self.playback
            .lock()
            .ok()
            .and_then(|slot| slot.as_ref().and_then(|handle| handle.level()))
    }

    fn reports_progress(&self) -> bool {
        true
    }

    fn progress(&self) -> Option<SpeechProgress> {
        let timeline = self.timeline.lock().ok().and_then(|slot| slot.clone());
        match timeline {
            Some(timeline) => {
                caption_timeline::progress(&self.speaking, &*timeline.lock().ok()?, &self.playback)
            }
            None => cloud_playback::progress(&self.speaking, &self.pieces, &self.playback),
        }
    }
}

impl AliyunBackend {
    fn begin(&self, request: TtsRequest, token: CancelToken) -> Result<(), TtsError> {
        let config = self.config()?;
        let spec = spec_for(&config.model);
        let sample_rate = negotiate_sample_rate_among(spec.sample_rates)
            .map_err(|err| TtsError::Backend(format!("{} ({})", err, err.code())))?;

        // Trimmed, not just trim-checked: a designed-voice id is pasted by
        // hand, and a trailing newline from the console would otherwise reach
        // the service verbatim and come back as the same 418 a wrong id gets.
        let voice = match request.voice.as_deref().map(str::trim) {
            Some(id) if !id.is_empty() => id.to_string(),
            _ => spec
                .default_voice()
                .map(str::to_string)
                .ok_or_else(|| {
                    TtsError::Backend(format!(
                        "{} has no system voices; configure a cloned or designed voice id",
                        spec.id
                    ))
                })?,
        };
        // Pitch is deliberately not sent. `pitch_rate` does apply, but halving
        // it stretched the audio to 4.2x rather than the 2x a resampling pitch
        // shift would give, so what it actually changes is unclear and the
        // settings page hides the row for cloud providers anyway.
        let speed = speed_from_normalized(request.rate);
        let gain = request.volume.unwrap_or(1.0);
        // Fast speech drains audio quicker than CosyVoice synthesizes it (the
        // service generates a constant characters-per-second regardless of the
        // requested rate), so those reads trade a moment of startup latency
        // for underruns that pause cleanly instead of crackling.
        let prebuffer = prebuffer_samples(speed, sample_rate);

        // One request per piece, all feeding the same sink: the service caps
        // a request, not a read, and the pieces end on sentence or clause
        // boundaries, so a seam is audible only as an ordinary pause.
        // `piece_chars` leads because CosyVoice's silent per-batch output
        // budget bites thousands of characters before `max_chars` would get
        // the request rejected outright; the `min` only guards a future spec
        // whose two limits drift past each other. Split before anything
        // starts, so `progress` can name the piece the sink is on.
        let instruction = Some(config.instruction.trim().to_string())
            .filter(|instruction| spec.accepts_instruction && !instruction.is_empty());

        let (split_limit, caption_limit) = piece_plan(spec, request.piece_limit);
        let written = cloud_playback::split_pieces(&request.text, split_limit, &self.pieces);
        let timeline =
            caption_limit.map(|_| Arc::new(Mutex::new(CaptionTimeline::new(sample_rate))));
        if let Ok(mut slot) = self.timeline.lock() {
            *slot = timeline.clone();
        }
        let pieces: Vec<SentPiece> = written
            .iter()
            .enumerate()
            .map(|(index, piece)| SentPiece::new(index, piece, caption_limit.zip(timeline.clone())))
            .collect();

        let (tx, rx) = mpsc::channel::<PieceStream>();
        // Lets the decode side tell "the provider failed" apart from "the audio
        // ended", which otherwise both look like a closed channel.
        let network_error: Arc<Mutex<Option<String>>> = Arc::new(Mutex::new(None));

        let decode_token = token.clone();
        let decode_error = network_error.clone();

        // Clear any handle left by the previous utterance before publishing the
        // slot, so a stop arriving now cannot reach a device we already closed.
        if let Ok(mut slot) = self.playback.lock() {
            *slot = None;
        }

        let playback_slot = self.playback.clone();
        // Raised by the decode thread when the first samples reach the device,
        // not here: a request in flight is not a sound, and the HUD tells those
        // two states apart.
        let speaking = self.speaking.clone();
        thread::Builder::new()
            .name("voicex-tts-cloud".to_string())
            .spawn(move || {
                cloud_playback::run_playback(
                    rx,
                    sample_rate,
                    gain,
                    prebuffer,
                    decode_token,
                    decode_error,
                    playback_slot,
                    speaking.clone(),
                );
                speaking.store(false, Ordering::SeqCst);
            })
            .map_err(|err| TtsError::Backend(format!("failed to spawn the decoder: {err}")))?;

        let http_token = token;
        let http_error = network_error;
        tauri::async_runtime::spawn(async move {
            for piece in &pieces {
                // Handed over before any of its bytes, so the sink can mark
                // where the piece begins in the sample stream.
                let (piece_tx, piece_rx) = mpsc::channel::<Vec<u8>>();
                if tx.send(piece_rx).is_err() {
                    // The sink is gone — it could not open the device, or the
                    // read was stopped — so nobody would hear the rest.
                    break;
                }
                let outcome = stream_audio_with_retry(
                    &config.api_key,
                    spec,
                    &Synthesis {
                        text: &piece.text,
                        voice: &voice,
                        sample_rate,
                        rate: speed,
                        instruction: instruction.as_deref(),
                        word_timestamps: piece.captions.is_some(),
                    },
                    &piece_tx,
                    piece.captions.as_ref(),
                    &http_token,
                )
                .await;
                // Closing the piece is what lets the sink move on to the next.
                drop(piece_tx);
                match outcome {
                    Ok(true) => {}
                    // Cancelled, or the decoder hung up: synthesizing the
                    // remaining pieces would only bill text nobody hears.
                    Ok(false) => break,
                    Err(err) => {
                        if let Ok(mut slot) = http_error.lock() {
                            *slot = Some(err);
                        }
                        break;
                    }
                }
            }
            // Closing the channel is what ends the sink's loop.
            drop(tx);
        });

        Ok(())
    }
}

/// How a read is split into requests, and the caption size when the service
/// times the captions itself.
///
/// With captions on, a model is split into caption-sized requests, one
/// caption each — except one that times its own speech: that read stays in as
/// few requests as the service takes, and its captions are placed by the
/// service's clock, so no cut of ours lands anywhere the service's own
/// chunking would not.
fn piece_plan(spec: &ModelSpec, caption_limit: Option<usize>) -> (usize, Option<usize>) {
    let own_limit = spec.piece_chars.min(spec.max_chars);
    match caption_limit.filter(|_| spec.timed_chunks) {
        Some(limit) => (own_limit, Some(limit)),
        None => (piece_limit_for(caption_limit, own_limit), None),
    }
}

/// One request of a read: the text sent, and for a read whose captions the
/// service times, what placing them needs.
struct SentPiece {
    text: String,
    captions: Option<CaptionFeed>,
}

impl SentPiece {
    /// `captions` is the caption size and the read's timeline, for a timed
    /// read.
    fn new(
        request: usize,
        written: &str,
        captions: Option<(usize, Arc<Mutex<CaptionTimeline>>)>,
    ) -> Self {
        let written: Vec<char> = written.chars().collect();
        let kept = number_spacing_kept(&written);
        let text: String = kept.iter().map(|&index| written[index]).collect();
        let captions = captions.map(|(limit, timeline)| {
            let mut kept = kept;
            kept.push(written.len());
            CaptionFeed {
                request,
                sent: text.chars().collect(),
                written,
                kept,
                limit,
                timeline,
            }
        });
        Self { text, captions }
    }
}

/// What a timed request needs to place its captions.
struct CaptionFeed {
    request: usize,
    /// The piece as written, which is what captions show.
    written: Vec<char>,
    /// The text sent: `written` less the spaces [`number_spacing_kept`]
    /// drops. The service echoes it back chunk by chunk.
    sent: Vec<char>,
    /// Where each character of `sent` sits in `written`, then `written.len()`.
    kept: Vec<usize>,
    /// Longest caption, in characters.
    limit: usize,
    timeline: Arc<Mutex<CaptionTimeline>>,
}

impl CaptionFeed {
    fn place(&self, start_ms: u64, text: String) {
        if let Ok(mut timeline) = self.timeline.lock() {
            timeline.push(self.request, start_ms, text);
        }
    }
}

/// One attempt at a timed request, following the service through its chunks.
///
/// A chunk's first caption goes up as soon as the chunk begins, starting
/// where the previous chunk's speech ended; the timing of the rest arrives
/// with the chunk's last frame, which the service sends long before playback
/// gets there (it renders at ~12x realtime).
struct ChunkReader<'a> {
    feed: &'a CaptionFeed,
    /// Characters of `feed.sent` the chunks so far have covered.
    cursor: usize,
    /// When the last finished chunk's speech ended, in request time.
    spoken_until: u64,
    /// The chunk being synthesized, as written, and its captions.
    open: Option<(Vec<char>, Vec<String>)>,
}

impl<'a> ChunkReader<'a> {
    fn new(feed: &'a CaptionFeed) -> Self {
        if let Ok(mut timeline) = feed.timeline.lock() {
            timeline.begin_request(feed.request);
        }
        Self {
            feed,
            cursor: 0,
            spoken_until: 0,
            open: None,
        }
    }

    fn read(&mut self, event: ChunkEvent) {
        match event {
            ChunkEvent::Begin(text) => self.begin(&text),
            ChunkEvent::End(words) => self.end(&words),
        }
    }

    fn begin(&mut self, sent: &str) {
        // A chunk that never reported its end still gets its captions up.
        self.finish();
        let feed = self.feed;
        let chunk: Vec<char> = sent.chars().collect();
        let end = self.cursor + chunk.len();
        let written = if feed.sent.get(self.cursor..end) == Some(&chunk[..]) {
            feed.written[feed.kept[self.cursor]..feed.kept[end]].to_vec()
        } else {
            // The echo is what lets a caption show the text as written. Off
            // its track, the service's own text is still what is being said.
            log_event(
                "caption_chunk_mismatch",
                &[
                    ("offset", self.cursor.to_string()),
                    ("chars", chunk.len().to_string()),
                ],
            );
            chunk
        };
        self.cursor = end;
        let captions = split_for_backend(&written.iter().collect::<String>(), feed.limit);
        feed.place(self.spoken_until, captions[0].clone());
        self.open = Some((written, captions));
    }

    fn end(&mut self, words: &[SpokenWord]) {
        let Some((written, captions)) = self.open.take() else {
            return;
        };
        let spoken: Vec<(char, u64)> = words
            .iter()
            .flat_map(|word| word.text.chars().map(move |ch| (ch, word.begin_ms)))
            .collect();
        let cuts: Vec<usize> = captions
            .iter()
            .scan(0, |at, caption| {
                *at += caption.chars().count();
                Some(*at)
            })
            .take(captions.len() - 1)
            .collect();
        let times = caption_timeline::cut_times(&written, &cuts, &spoken);
        // Nothing recognizable after a cut: its caption comes up with the
        // chunk's last word rather than never.
        let last_word = words.last().map_or(self.spoken_until, |word| word.begin_ms);
        let untimed = times.iter().filter(|time| time.is_none()).count();
        let count = captions.len();
        for (caption, time) in captions.into_iter().skip(1).zip(times) {
            self.feed.place(time.unwrap_or(last_word), caption);
        }
        if let Some(last) = words.last() {
            self.spoken_until = last.end_ms;
        }
        log_event(
            "caption_chunk",
            &[
                ("chars", written.len().to_string()),
                ("captions", count.to_string()),
                ("untimed", untimed.to_string()),
            ],
        );
    }

    /// Place whatever the open chunk still holds, timed or not.
    fn finish(&mut self) {
        if self.open.is_some() {
            self.end(&[]);
        }
    }
}

/// Stream one synthesis response, forwarding decoded audio bytes to `tx` and,
/// for a timed request, placing its captions as the service reports its
/// chunks.
///
/// `Ok(true)` means the piece completed and the caller may stream the next
/// one; `Ok(false)` means the read was cancelled or the decoder hung up, so
/// further pieces would only bill text nobody hears.
async fn stream_audio_with_retry(
    api_key: &str,
    spec: &ModelSpec,
    synthesis: &Synthesis<'_>,
    tx: &Sender<Vec<u8>>,
    captions: Option<&CaptionFeed>,
    token: &CancelToken,
) -> Result<bool, String> {
    let mut retries_done = 0;
    loop {
        match stream_audio(api_key, spec, synthesis, tx, captions, token).await {
            Ok(outcome) => return Ok(outcome),
            Err(err) => {
                if token.is_cancelled() {
                    return Ok(false);
                }
                if let Some(delay_ms) = err.retry_delay_ms(retries_done) {
                    log_cloud_retry("aliyun", retries_done, delay_ms, err.reason());
                    tokio::time::sleep(std::time::Duration::from_millis(delay_ms)).await;
                    if token.is_cancelled() {
                        return Ok(false);
                    }
                    retries_done += 1;
                    continue;
                }
                return Err(err.into_detail());
            }
        }
    }
}

async fn stream_audio(
    api_key: &str,
    spec: &ModelSpec,
    synthesis: &Synthesis<'_>,
    tx: &Sender<Vec<u8>>,
    captions: Option<&CaptionFeed>,
    token: &CancelToken,
) -> Result<bool, CloudStreamError> {
    let response = cloud_http_client()?
        .post(format!("{HOST}{}", spec.path))
        .header("Authorization", format!("Bearer {api_key}"))
        // Without this the service synthesizes the whole text before replying,
        // which for a long selection takes minutes rather than seconds.
        .header("X-DashScope-SSE", "enable")
        .json(&(spec.build_body)(spec.id, synthesis))
        .send()
        .await
        .map_err(CloudStreamError::request)?;

    let status = response.status();
    if !status.is_success() {
        let detail = response.text().await.unwrap_or_default();
        return Err(CloudStreamError::http(status, describe_failure(&detail)));
    }

    let mut stream = response.bytes_stream();
    let mut pending = Vec::<u8>::new();
    let mut audio_emitted = false;
    // Per attempt: a retry is sent the whole text again, chunks and all.
    let mut chunks = captions.map(ChunkReader::new);

    while let Some(chunk) = stream.next().await {
        if token.is_cancelled() {
            return Ok(false);
        }
        let chunk = chunk.map_err(|err| CloudStreamError::stream(err, audio_emitted))?;
        pending.extend_from_slice(&chunk);

        // Server-sent events are line-oriented and a chunk may split a line.
        while let Some(newline) = pending.iter().position(|byte| *byte == b'\n') {
            let line: Vec<u8> = pending.drain(..=newline).collect();
            let frame = parse_line(&line[..line.len() - 1])
                .map_err(|err| CloudStreamError::response(err, audio_emitted))?;
            if let (Some(chunks), Some(event)) = (chunks.as_mut(), frame.chunk) {
                chunks.read(event);
            }
            if let Some(audio) = frame.audio {
                if tx.send(audio).is_err() {
                    // The decoder is gone; nothing left to stream into.
                    return Ok(false);
                }
                audio_emitted = true;
            }
        }
    }

    if !pending.is_empty() {
        let frame =
            parse_line(&pending).map_err(|err| CloudStreamError::response(err, audio_emitted))?;
        if let (Some(chunks), Some(event)) = (chunks.as_mut(), frame.chunk) {
            chunks.read(event);
        }
        if let Some(audio) = frame.audio {
            if tx.send(audio).is_err() {
                return Ok(false);
            }
        }
    }
    if let Some(chunks) = chunks.as_mut() {
        chunks.finish();
    }

    Ok(true)
}

/// One word as the service times it. Punctuation rides on the word after it:
/// a sentence's first word reads `"。直"`.
#[derive(Debug, Clone, PartialEq)]
struct SpokenWord {
    text: String,
    begin_ms: u64,
    end_ms: u64,
}

/// The service announcing its own chunks of a request (`output.type`).
#[derive(Debug, Clone, PartialEq)]
enum ChunkEvent {
    /// `sentence-begin`, with the chunk's text as it was sent.
    Begin(String),
    /// `sentence-end`, with every word of the chunk when timestamps were
    /// asked for.
    End(Vec<SpokenWord>),
}

/// What one line of the stream carries.
#[derive(Debug, Default)]
struct Frame {
    audio: Option<Vec<u8>>,
    chunk: Option<ChunkEvent>,
}

/// Decode one SSE line into its audio bytes and chunk event, either of which
/// may be absent.
///
/// Everything but `data:` is framing — `id:`, `event:`, the `:HTTP_STATUS/200`
/// comment, and the blank line between events.
fn parse_line(line: &[u8]) -> Result<Frame, String> {
    let text = std::str::from_utf8(line).map_err(|_| "response was not UTF-8".to_string())?;
    let Some(payload) = text.trim_end_matches('\r').strip_prefix("data:") else {
        return Ok(Frame::default());
    };
    let payload = payload.trim_start();
    if payload.is_empty() {
        return Ok(Frame::default());
    }

    let value: Value =
        serde_json::from_str(payload).map_err(|err| format!("malformed response: {err}"))?;

    // Failures arrive mid-stream with the same 200 as the audio frames, so the
    // only signal is this field appearing.
    if let Some(code) = value.get("code").and_then(|code| code.as_str()) {
        let message = value
            .get("message")
            .and_then(|message| message.as_str())
            .unwrap_or("unknown error");
        return Err(format!("provider error {code}: {message}"));
    }

    let audio = match value
        .pointer("/output/audio/data")
        .and_then(|data| data.as_str())
    {
        // The final frame carries the finished file's URL and an empty `data`.
        // We already have every byte it points at.
        Some(data) if !data.is_empty() => Some(
            base64::engine::general_purpose::STANDARD
                .decode(data)
                .map_err(|err| format!("bad base64 in response: {err}"))?,
        ),
        _ => None,
    };
    Ok(Frame {
        audio,
        chunk: chunk_event(&value),
    })
}

fn chunk_event(value: &Value) -> Option<ChunkEvent> {
    let output = value.get("output")?;
    match output.get("type")?.as_str()? {
        "sentence-begin" => Some(ChunkEvent::Begin(
            output
                .get("original_text")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string(),
        )),
        "sentence-end" => {
            let words = output
                .pointer("/sentence/words")
                .and_then(Value::as_array)
                .map(|words| {
                    words
                        .iter()
                        .filter_map(|word| {
                            Some(SpokenWord {
                                text: word.get("text")?.as_str()?.to_string(),
                                begin_ms: word.get("begin_time")?.as_u64()?,
                                end_ms: word.get("end_time")?.as_u64()?,
                            })
                        })
                        .collect()
                })
                .unwrap_or_default();
            Some(ChunkEvent::End(words))
        }
        _ => None,
    }
}

/// Pull the useful part out of an error body, falling back to its first line.
fn describe_failure(body: &str) -> String {
    serde_json::from_str::<Value>(body)
        .ok()
        .and_then(|value| {
            let code = value.get("code").and_then(|code| code.as_str())?;
            let message = value
                .get("message")
                .and_then(|message| message.as_str())
                .unwrap_or("");
            Some(format!("{code}: {message}"))
        })
        .unwrap_or_else(|| body.lines().next().unwrap_or_default().to_string())
        .chars()
        .take(200)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_rate_scales_line_up_at_both_ends_and_the_middle() {
        // Stored 0..1 around a 0.5 default, shown as 0.5x-2x, sent as the same
        // 0.5-2.0 multiplier. Getting this wrong is silent: the voice just
        // speaks at the wrong speed.
        assert_eq!(speed_from_normalized(Some(0.25)), 0.5, "0.5x");
        assert_eq!(speed_from_normalized(Some(0.5)), 1.0, "1x is neutral");
        assert_eq!(speed_from_normalized(Some(1.0)), 2.0, "2x");
        assert_eq!(speed_from_normalized(None), 1.0, "unset is neutral");
    }

    #[test]
    fn rates_outside_the_slider_are_clamped_to_what_the_provider_accepts() {
        assert_eq!(speed_from_normalized(Some(0.0)), 0.5);
        assert_eq!(speed_from_normalized(Some(5.0)), 2.0);
    }

    #[test]
    fn spaces_around_numbers_are_closed_before_synthesis() {
        let sent = |text: &str| SentPiece::new(0, text, None).text;
        // Read as 「共、六百九十八、片」 with the spaces in.
        assert_eq!(
            sent("共 698 片，从 M3 版本起，延迟 300 ms。"),
            "共698片，从M3版本起，延迟300 ms。"
        );
        // Letter-only words sound the same either way, and spaces between
        // two ASCII tokens are the text's own.
        let words = "使用 git commit --amend 即可，CORE 不变。";
        assert_eq!(sent(words), words);
        assert_eq!(sent(" 8 "), " 8 ");
    }

    #[test]
    fn each_model_spells_its_parameters_its_own_way() {
        // The two services ignore each other's spelling silently — 200 and
        // audio at the default rate — so a mix-up here would be audible only as
        // "the speed slider does nothing".
        let synthesis = Synthesis {
            text: "hi",
            voice: "Cherry",
            sample_rate: 24_000,
            rate: 1.5,
            instruction: None,
            word_timestamps: false,
        };

        let qwen3 = (spec_for(MODEL_QWEN3).build_body)(MODEL_QWEN3, &synthesis);
        assert_eq!(qwen3["parameters"]["speech_rate"], 1.5);
        assert_eq!(qwen3["parameters"]["response_format"], "mp3");
        assert_eq!(qwen3["input"]["language_type"], "Auto");
        assert!(qwen3["input"].get("format").is_none());

        let audio = (spec_for(MODEL_QWEN_AUDIO).build_body)(MODEL_QWEN_AUDIO, &synthesis);
        assert_eq!(audio["input"]["rate"], 1.5);
        assert_eq!(audio["input"]["format"], "mp3");
        assert!(audio["input"].get("language_type").is_none());
        assert!(audio.get("parameters").is_none());

        let audio31 = (spec_for(MODEL_QWEN_AUDIO_31).build_body)(MODEL_QWEN_AUDIO_31, &synthesis);
        assert_eq!(audio31["model"], MODEL_QWEN_AUDIO_31);
        assert_eq!(audio31["input"]["rate"], 1.5);
        assert_eq!(audio31["input"]["format"], "mp3");
        assert_eq!(
            spec_for(MODEL_QWEN_AUDIO_31).path,
            spec_for(MODEL_QWEN_AUDIO).path
        );

        let cosy = (spec_for(MODEL_COSYVOICE).build_body)(MODEL_COSYVOICE, &synthesis);
        assert_eq!(cosy["model"], MODEL_COSYVOICE);
        assert_eq!(cosy["input"]["rate"], 1.5);
        assert_eq!(cosy["input"]["format"], "mp3");
        assert_eq!(
            spec_for(MODEL_COSYVOICE).path,
            spec_for(MODEL_QWEN_AUDIO).path
        );

        let cosy35 = (spec_for(MODEL_COSYVOICE_V35).build_body)(MODEL_COSYVOICE_V35, &synthesis);
        assert_eq!(cosy35["model"], MODEL_COSYVOICE_V35);
        assert_eq!(cosy35["input"]["rate"], 1.5);
        assert_eq!(cosy35["input"]["format"], "mp3");
        assert_eq!(
            spec_for(MODEL_COSYVOICE_V35).path,
            spec_for(MODEL_COSYVOICE).path
        );
    }

    #[test]
    fn the_instruction_reaches_only_the_models_that_accept_it() {
        // CosyVoice v3's system voices answer free text with an engine error
        // (428), which would fail every read for a setting the user may have
        // typed for another model; the others take it in `input`.
        for (model, accepts) in [
            (MODEL_QWEN3, false),
            (MODEL_QWEN_AUDIO, true),
            (MODEL_QWEN_AUDIO_31, true),
            (MODEL_COSYVOICE, false),
            (MODEL_COSYVOICE_V35, true),
        ] {
            assert_eq!(spec_for(model).accepts_instruction, accepts, "{model}");
        }

        let with = Synthesis {
            text: "hi",
            voice: "anxiaolan_v3.1",
            sample_rate: 24_000,
            rate: 1.0,
            instruction: Some(DEFAULT_INSTRUCTION),
            word_timestamps: false,
        };
        let body = (spec_for(MODEL_QWEN_AUDIO_31).build_body)(MODEL_QWEN_AUDIO_31, &with);
        assert_eq!(body["input"]["instruction"], DEFAULT_INSTRUCTION);

        let without = Synthesis { instruction: None, ..with };
        let body = (spec_for(MODEL_QWEN_AUDIO_31).build_body)(MODEL_QWEN_AUDIO_31, &without);
        assert!(body["input"].get("instruction").is_none());
    }

    #[test]
    fn every_spec_names_its_own_model_in_the_body() {
        // The id decides which spec is looked up; the body decides which model
        // the service runs. `stream_audio` threads `spec.id` into `build_body`,
        // and this pins that a body cannot disagree with its spec.
        let synthesis = Synthesis {
            text: "hi",
            voice: "Cherry",
            sample_rate: 24_000,
            rate: 1.0,
            instruction: None,
            word_timestamps: false,
        };
        for spec in &SPECS {
            assert_eq!((spec.build_body)(spec.id, &synthesis)["model"], spec.id);
        }
    }

    #[test]
    fn qwen3_never_asks_for_a_rate_it_cannot_render() {
        // The playback module's own list includes 44.1 and 22.05 kHz, which
        // this model does not render. Asking anyway comes back as audio at some
        // other rate and surfaces as a decode mismatch, not as speech.
        for rate in spec_for(MODEL_QWEN3).sample_rates {
            assert!(
                matches!(rate, 48_000 | 24_000 | 16_000 | 8_000),
                "{rate} Hz is not a qwen3-tts-flash rate"
            );
        }
    }

    #[test]
    fn audio_frames_yield_bytes_and_framing_lines_yield_none() {
        let audio = parse_line(br#"data:{"output":{"audio":{"data":"aGVsbG8="}}}"#)
            .unwrap()
            .audio
            .unwrap();
        assert_eq!(audio, b"hello");

        // The final frame points at the finished file; we already have it.
        let last =
            parse_line(br#"data:{"output":{"audio":{"data":"","url":"http://x"}}}"#).unwrap();
        assert!(last.audio.is_none());

        for framing in [
            &b"id:1"[..],
            b"event:result",
            b":HTTP_STATUS/200",
            b"",
            b"data:",
        ] {
            let frame = parse_line(framing).unwrap();
            assert!(
                frame.audio.is_none() && frame.chunk.is_none(),
                "{framing:?}"
            );
        }
    }

    #[test]
    fn only_qwen_audio_31_times_its_captions() {
        let timed: Vec<&str> = SPECS
            .iter()
            .filter(|spec| spec.timed_chunks)
            .map(|spec| spec.id)
            .collect();
        assert_eq!(timed, vec![MODEL_QWEN_AUDIO_31]);

        let synthesis = Synthesis {
            text: "hi",
            voice: "anxiaolan_v3.1",
            sample_rate: 24_000,
            rate: 1.0,
            instruction: None,
            word_timestamps: true,
        };
        let body = (spec_for(MODEL_QWEN_AUDIO_31).build_body)(MODEL_QWEN_AUDIO_31, &synthesis);
        assert_eq!(body["input"]["word_timestamp_enabled"], true);
        let untimed = Synthesis {
            word_timestamps: false,
            ..synthesis
        };
        let body = (spec_for(MODEL_QWEN_AUDIO_31).build_body)(MODEL_QWEN_AUDIO_31, &untimed);
        assert!(body["input"].get("word_timestamp_enabled").is_none());
    }

    #[test]
    fn only_a_timed_model_keeps_a_captioned_read_in_one_request() {
        let plan = |model, captions| piece_plan(spec_for(model), captions);
        assert_eq!(plan(MODEL_QWEN_AUDIO_31, Some(120)), (9_000, Some(120)));
        assert_eq!(plan(MODEL_QWEN_AUDIO_31, None), (9_000, None));
        // Every other model is split into caption-sized requests as before.
        assert_eq!(plan(MODEL_QWEN_AUDIO, Some(120)), (120, None));
        assert_eq!(plan(MODEL_QWEN3, Some(120)), (120, None));
        assert_eq!(plan(MODEL_COSYVOICE, None), (120, None));
    }

    #[test]
    fn chunk_frames_yield_their_text_and_timing() {
        let begin = parse_line(
            r#"data:{"output":{"type":"sentence-begin","original_text":"你好。","sentence":{"index":0}}}"#
                .as_bytes(),
        )
        .unwrap();
        assert_eq!(begin.chunk, Some(ChunkEvent::Begin("你好。".to_string())));

        let end = parse_line(
            r#"data:{"output":{"type":"sentence-end","sentence":{"index":0,"words":[{"text":"你","begin_time":200,"end_time":400,"begin_index":0,"end_index":1},{"text":"好","begin_time":400,"end_time":650,"begin_index":1,"end_index":2}]}}}"#
                .as_bytes(),
        )
        .unwrap();
        let word = |text: &str, begin_ms, end_ms| SpokenWord {
            text: text.to_string(),
            begin_ms,
            end_ms,
        };
        assert_eq!(
            end.chunk,
            Some(ChunkEvent::End(vec![
                word("你", 200, 400),
                word("好", 400, 650)
            ]))
        );
    }

    /// A timed request's feed and timeline, at 1 kHz so samples read as ms.
    fn feed(written: &str, limit: usize) -> (SentPiece, Arc<Mutex<CaptionTimeline>>) {
        let timeline = Arc::new(Mutex::new(CaptionTimeline::new(1_000)));
        (
            SentPiece::new(0, written, Some((limit, timeline.clone()))),
            timeline,
        )
    }

    fn spoken(list: &[(&str, u64)], end_ms: u64) -> Vec<SpokenWord> {
        let mut words: Vec<SpokenWord> = list
            .iter()
            .map(|&(text, begin_ms)| SpokenWord {
                text: text.to_string(),
                begin_ms,
                end_ms: begin_ms + 100,
            })
            .collect();
        if let Some(last) = words.last_mut() {
            last.end_ms = end_ms;
        }
        words
    }

    #[test]
    fn a_timed_chunk_captions_the_text_as_written_by_the_services_clock() {
        let (piece, timeline) = feed("共 698 片，最短的 24 字。直观地说：折返更早。", 20);
        // The service echoes the spaces-closed text it was sent.
        assert_eq!(piece.text, "共698片，最短的24字。直观地说：折返更早。");
        let mut chunks = ChunkReader::new(piece.captions.as_ref().unwrap());
        chunks.read(ChunkEvent::Begin(piece.text.clone()));
        let heard = |ms| {
            timeline
                .lock()
                .unwrap()
                .at(0, ms)
                .map(|p| (p.index, p.text))
        };
        // Up from the start, before any timing has arrived.
        assert_eq!(
            heard(10),
            Some((0, "共 698 片，最短的 24 字。".to_string()))
        );

        chunks.read(ChunkEvent::End(spoken(
            &[
                ("共", 0),
                ("六", 100),
                ("百", 150),
                ("九", 200),
                ("十", 250),
                ("八", 300),
                ("片", 400),
                ("，最", 600),
                ("短", 700),
                ("的", 800),
                ("二", 900),
                ("十", 950),
                ("四", 1_000),
                ("字", 1_100),
                ("。直", 1_500),
                ("观", 1_600),
                ("说", 1_700),
                ("：折", 1_900),
                ("返", 2_000),
                ("更", 2_100),
                ("早", 2_200),
            ],
            2_400,
        )));
        assert_eq!(heard(1_400).unwrap().0, 0);
        assert_eq!(heard(1_500), Some((1, "直观地说：折返更早。".to_string())));
    }

    #[test]
    fn the_next_chunk_opens_where_the_last_ones_speech_ended() {
        let (piece, timeline) = feed("第一句。第二句。", 120);
        let mut chunks = ChunkReader::new(piece.captions.as_ref().unwrap());
        chunks.read(ChunkEvent::Begin("第一句。".to_string()));
        chunks.read(ChunkEvent::End(spoken(
            &[("第", 0), ("一", 100), ("句", 200)],
            900,
        )));
        chunks.read(ChunkEvent::Begin("第二句。".to_string()));
        let heard = |ms| timeline.lock().unwrap().at(0, ms).map(|p| p.text);
        assert_eq!(heard(899).as_deref(), Some("第一句。"));
        assert_eq!(heard(900).as_deref(), Some("第二句。"));

        // A retry is sent the text again; what the failed attempt placed goes.
        let _retry = ChunkReader::new(piece.captions.as_ref().unwrap());
        assert_eq!(heard(900), None);
    }

    #[test]
    fn a_mid_stream_failure_is_reported_rather_than_read_as_end_of_audio() {
        // These arrive with the same HTTP 200 as the audio frames, so missing
        // one would look like a read that simply stopped early.
        let err = parse_line(
            br#"data:{"code":"InvalidParameter","message":"Invalid voice specified","request_id":"x"}"#,
        )
        .unwrap_err();
        assert!(err.contains("InvalidParameter"), "{err}");
        assert!(err.contains("Invalid voice"), "{err}");
    }

    #[test]
    fn a_carriage_return_does_not_hide_the_payload() {
        // CRLF framing would otherwise leave a trailing \r inside the JSON.
        let audio = parse_line(b"data:{\"output\":{\"audio\":{\"data\":\"aGk=\"}}}\r")
            .unwrap()
            .audio
            .unwrap();
        assert_eq!(audio, b"hi");
    }

    #[test]
    fn http_failures_report_the_provider_code_not_the_raw_body() {
        let described = describe_failure(
            r#"{"code":"InvalidApiKey","message":"Invalid API-key provided.","request_id":"x"}"#,
        );
        assert_eq!(described, "InvalidApiKey: Invalid API-key provided.");
        assert_eq!(
            describe_failure("gateway timeout\nsecond line"),
            "gateway timeout"
        );
    }

    #[test]
    fn an_unknown_model_falls_back_rather_than_panicking() {
        // Settings are user-editable and survive downgrades, so a model string
        // this build has never heard of has to resolve to something.
        assert_eq!(spec_for("qwen9-tts-imaginary").id, MODEL_QWEN3);
        assert_eq!(default_voice_for("qwen9-tts-imaginary"), "Cherry");
        assert_eq!(default_voice_for(MODEL_QWEN_AUDIO), "longanfengyue");
        assert_eq!(default_voice_for(MODEL_QWEN_AUDIO_31), "anxiaolan_v3.1");
        assert_eq!(default_voice_for(MODEL_COSYVOICE), "longanyang");
        assert_eq!(
            default_voice_for(MODEL_COSYVOICE_V35),
            "",
            "v3.5-flash has no system voice to fall back to"
        );
    }

    #[test]
    fn switching_model_switches_the_voice_table_with_it() {
        // What the settings page's voice picker rides on: it re-asks after a
        // model change, and the controller answers by re-applying the config
        // first. If the table did not follow, the picker would offer voices the
        // selected model rejects.
        let backend = AliyunBackend::new(AliyunConfig {
            api_key: "sk-test".to_string(),
            model: MODEL_QWEN3.to_string(),
            instruction: String::new(),
        });
        let listed = backend.list_voices().unwrap();
        assert!(listed.iter().any(|voice| voice.id == "Cherry"));
        assert!(!listed.iter().any(|voice| voice.id == "longanfengyue"));

        backend.apply_config(AliyunConfig {
            api_key: "sk-test".to_string(),
            model: MODEL_QWEN_AUDIO.to_string(),
            instruction: String::new(),
        });
        let listed = backend.list_voices().unwrap();
        assert!(listed.iter().any(|voice| voice.id == "longanfengyue"));
        assert!(!listed.iter().any(|voice| voice.id == "Cherry"));
        assert!(!listed.iter().any(|voice| voice.id == "longanyang"));

        // 3.1 rejects every 3.0 id (`Engine error [411]`), so the tables
        // must not overlap at all.
        backend.apply_config(AliyunConfig {
            api_key: "sk-test".to_string(),
            model: MODEL_QWEN_AUDIO_31.to_string(),
            instruction: String::new(),
        });
        let listed = backend.list_voices().unwrap();
        assert!(listed.iter().any(|voice| voice.id == "anxiaolan_v3.1"));
        assert!(listed.iter().all(|voice| voice.id.ends_with("_v3.1")));
        assert!(!listed.iter().any(|voice| voice.id == "longanfengyue"));

        backend.apply_config(AliyunConfig {
            api_key: "sk-test".to_string(),
            model: MODEL_COSYVOICE.to_string(),
            instruction: String::new(),
        });
        let listed = backend.list_voices().unwrap();
        assert!(listed.iter().any(|voice| voice.id == "longanyang"));
        assert!(!listed.iter().any(|voice| voice.id == "longanfengyue"));
        assert!(!listed.iter().any(|voice| voice.id == "Cherry"));

        backend.apply_config(AliyunConfig {
            api_key: "sk-test".to_string(),
            model: MODEL_COSYVOICE_V35.to_string(),
            instruction: String::new(),
        });
        let listed = backend.list_voices().unwrap();
        assert!(
            listed.is_empty(),
            "v3.5-flash must not list system voices the service will 418"
        );

        // The per-request limit rides along with the model; `begin` splits a
        // longer selection into pieces of at most this size.
        assert_eq!(spec_for(MODEL_COSYVOICE).max_chars, 9_000);
        assert_eq!(spec_for(MODEL_COSYVOICE_V35).max_chars, 9_000);
    }

    #[test]
    fn only_cosyvoice_needs_the_short_piece_limit() {
        // The ~836-token batch budget was reproduced on cosyvoice-v3-flash
        // (2026-08) and again on v3.5-flash with a designed voice (2026-09:
        // 180 characters complete, 200 characters an empty ID3). The same
        // 163-character single-sentence text came back complete from
        // qwen3-tts-flash and qwen-audio-3.0-tts-flash, and a 3000-character
        // one from qwen-audio-3.1-tts-flash. Capping the others
        // would only multiply requests and seams for nothing — and a
        // cosyvoice limit above the measured cliff would reintroduce
        // silently truncated tails.
        assert!(
            spec_for(MODEL_COSYVOICE).piece_chars <= 150,
            "cosyvoice pieces must stay inside the silent output budget"
        );
        assert_eq!(
            spec_for(MODEL_COSYVOICE_V35).piece_chars,
            spec_for(MODEL_COSYVOICE).piece_chars,
            "v3.5-flash shares v3's engine and its margin until dense text is measured on it"
        );
        assert_eq!(spec_for(MODEL_QWEN3).piece_chars, 5_000);
        assert_eq!(spec_for(MODEL_QWEN_AUDIO).piece_chars, 9_000);
        assert_eq!(spec_for(MODEL_QWEN_AUDIO_31).piece_chars, 9_000);
        for spec in &SPECS {
            assert!(
                spec.piece_chars <= spec.max_chars,
                "{}: a piece the service rejects outright is never right",
                spec.id
            );
        }
    }

    #[test]
    fn a_refused_start_hands_the_session_back() {
        // Without this the session stays claimed forever: the read hotkey turns
        // into a permanent "stop", and the HUD sits on "preparing" with nothing
        // ever coming. Reachable by simply not having configured a key yet.
        let backend = AliyunBackend::new(AliyunConfig {
            api_key: String::new(),
            model: MODEL_QWEN3.to_string(),
            instruction: String::new(),
        });
        let slot = crate::tts::SessionSlot::default();
        let token = slot.claim();
        assert!(slot.is_active());

        let outcome = backend.start(TtsRequest::plain("hi".to_string()), token);

        assert!(outcome.is_err(), "an unconfigured backend must refuse");
        assert!(!slot.is_active(), "a refusal must release the session");
        assert_eq!(backend.status(), TtsStatus::Idle);
    }

    #[test]
    fn a_v35_start_without_a_voice_id_hands_the_session_back() {
        // v3.5-flash has an empty voice table, so a missing id cannot fall
        // back to a preset. The refusal has to release the session the same
        // way an empty API key does, or the HUD sits on preparing forever.
        let backend = AliyunBackend::new(AliyunConfig {
            api_key: "sk-test".to_string(),
            model: MODEL_COSYVOICE_V35.to_string(),
            instruction: String::new(),
        });
        let slot = crate::tts::SessionSlot::default();
        let token = slot.claim();
        let outcome = backend.start(TtsRequest::plain("hi".to_string()), token);
        let err = outcome.expect_err("a designed-voice model must refuse an empty id");
        assert!(err.to_string().contains("no system voices"), "{err}");
        assert!(!slot.is_active(), "a refusal must release the session");
        assert_eq!(backend.status(), TtsStatus::Idle);
    }

    /// End-to-end against the live service for both models: network, SSE parse,
    /// MP3 decode and playback, in the same arrangement the backend uses. Plays
    /// audio, so it is opt-in:
    ///
    /// ```text
    /// ALIYUN_TTS_API_KEY=... cargo test --lib aliyun::tests::live -- --ignored --nocapture
    /// ```
    #[test]
    #[ignore = "requires network access and credentials"]
    fn live_synthesis_decodes_and_plays() {
        use crate::tts::decode::{decode_mp3_stream, ChunkSource};
        use crate::tts::playback::Playback;
        use crate::tts::SessionSlot;
        use std::time::Instant;

        let api_key = std::env::var("ALIYUN_TTS_API_KEY").expect("ALIYUN_TTS_API_KEY is not set");

        for spec in SPECS.iter() {
            let Some(voice) = spec.default_voice() else {
                eprintln!(
                    "\n=== {} skipped (no system voices; designed/cloned id required) ===",
                    spec.id
                );
                continue;
            };
            let rate = negotiate_sample_rate_among(spec.sample_rates)
                .expect("no usable output sample rate");
            eprintln!("\n=== {} @ {rate} Hz, voice {voice} ===", spec.id);

            let text = "阿里云百炼语音合成，端到端链路验证：流式接收、MP3 解码与本地播放。";
            let slot = SessionSlot::default();
            let token = slot.claim();
            let (tx, rx) = mpsc::channel::<Vec<u8>>();
            let source = ChunkSource::new(rx, token.clone());

            let decode_token = token.clone();
            let decoder = thread::spawn(move || {
                let playback =
                    Playback::open(rate, 1.0, 0).expect("failed to open the output device");
                let handle = playback.handle();
                // Sampled while audio is playing, because this drives the HUD
                // waveform and a level that only appears after the last sample
                // would leave the bars hidden for the whole read.
                let mut levels: Vec<f32> = Vec::new();
                let samples = decode_mp3_stream(source, rate, |chunk| {
                    if let Some(level) = handle.level() {
                        levels.push(level);
                    }
                    playback.push(chunk)
                })
                .expect("decode failed");
                playback.mark_end_of_stream();
                playback
                    .wait_until_drained(&decode_token)
                    .expect("playback stalled");
                (samples, levels)
            });

            let started = Instant::now();
            let outcome = tauri::async_runtime::block_on(stream_audio(
                &api_key,
                spec,
                &Synthesis {
                    text,
                    voice,
                    sample_rate: rate,
                    rate: 1.0,
                    instruction: None,
                    word_timestamps: false,
                },
                &tx,
                None,
                &token,
            ));
            drop(tx);
            outcome.expect("streaming failed");

            let (samples, levels) = decoder.join().expect("decoder panicked");
            let seconds = samples as f64 / rate as f64;
            let peak = levels.iter().copied().fold(0.0f32, f32::max);
            eprintln!(
                "decoded {samples} samples ({seconds:.2} s) in {:?}, peak level {peak:.4}",
                started.elapsed()
            );

            assert!(
                seconds > 2.0,
                "{}: expected several seconds of speech, decoded {seconds:.2} s",
                spec.id
            );
            assert!(
                peak > 0.01,
                "{}: the HUD waveform is driven by this level, and it stayed at {peak:.4} \
                 while audio was playing",
                spec.id
            );
        }
    }

    /// The timed caption path end to end: Qwen-Audio 3.1 with captions on,
    /// polled the way the HUD driver polls it. Plays audio (quietly), so it is
    /// opt-in:
    ///
    /// ```text
    /// ALIYUN_TTS_API_KEY=... cargo test --lib aliyun::tests::live_timed -- --ignored --nocapture
    /// ```
    #[test]
    #[ignore = "requires network access and credentials"]
    fn live_timed_captions_follow_the_service() {
        use crate::tts::SessionSlot;
        use std::time::{Duration, Instant};

        let api_key = std::env::var("ALIYUN_TTS_API_KEY").expect("ALIYUN_TTS_API_KEY is not set");
        let backend = AliyunBackend::new(AliyunConfig {
            api_key,
            model: MODEL_QWEN_AUDIO_31.to_string(),
            instruction: DEFAULT_INSTRUCTION.to_string(),
        });
        // Sentences long enough that the service packs chunks past the
        // caption size, so some captions are placed inside a chunk.
        let text = "这次一共测了 25 句历史文本，其中 15 句带有数字，另外 10 句只含英文单词。\
            服务端会把中文与数字之间的空格读成停顿，比如把每种配置有 8 个固定起点读成有、八、个。\
            去掉这些空格以后，插入的顿号从 18 处降到 1 处，停顿也从 98 处减少到 83 处。\
            纯英文单词两侧的空格不会产生顿号，所以 CORE 和 Agent 这样的词保持原样。\
            字幕仍然显示原来的文字，只有发给服务端的文本去掉了空格。\
            整段文字现在只发一次请求，字幕的切换时间来自服务端返回的逐字时间戳。";

        let slot = SessionSlot::default();
        let mut request = TtsRequest::plain(text.to_string());
        request.piece_limit = Some(120);
        request.rate = Some(0.6);
        request.volume = Some(0.2);
        backend.start(request, slot.claim()).expect("start");

        let started = Instant::now();
        let mut shown: Vec<SpeechProgress> = Vec::new();
        while slot.is_active() && started.elapsed() < Duration::from_secs(180) {
            if let Some(now) = backend.progress() {
                if shown.last() != Some(&now) {
                    eprintln!(
                        "{:6.2}s  caption {} ({} chars): {}",
                        started.elapsed().as_secs_f64(),
                        now.index,
                        now.text.chars().count(),
                        now.text
                    );
                    shown.push(now);
                }
            }
            thread::sleep(Duration::from_millis(60));
        }

        assert!(!slot.is_active(), "the read did not finish in time");
        let indices: Vec<usize> = shown.iter().map(|caption| caption.index).collect();
        assert_eq!(
            indices,
            (0..shown.len()).collect::<Vec<_>>(),
            "captions skipped or repeated"
        );
        let joined: String = shown.iter().map(|caption| caption.text.as_str()).collect();
        assert_eq!(
            joined, text,
            "captions must cover the text as written, spaces and all"
        );
        assert!(shown
            .iter()
            .all(|caption| caption.text.chars().count() <= 120));
        assert!(shown.len() > 1);
    }

    /// Which voice ids the account actually accepts. Voice tables are hand-kept
    /// (there is no list endpoint), and a wrong id fails only at speak time with
    /// `Invalid voice specified` — so this checks them all in one pass:
    ///
    /// ```text
    /// ALIYUN_TTS_API_KEY=... cargo test --lib aliyun::tests::voice_table -- --ignored --nocapture
    /// ```
    #[test]
    #[ignore = "requires network access and credentials"]
    fn voice_table_matches_what_the_service_accepts() {
        let api_key = std::env::var("ALIYUN_TTS_API_KEY").expect("ALIYUN_TTS_API_KEY is not set");
        let mut rejected: Vec<String> = Vec::new();

        for spec in SPECS.iter() {
            for (id, name, _) in spec.presets() {
                let slot = crate::tts::SessionSlot::default();
                let token = slot.claim();
                let (tx, rx) = mpsc::channel::<Vec<u8>>();
                // Drained on a thread so a slow reader cannot stall the sender.
                let drain = thread::spawn(move || rx.iter().count());

                let outcome = tauri::async_runtime::block_on(stream_audio(
                    &api_key,
                    spec,
                    &Synthesis {
                        text: "测试。",
                        voice: id,
                        sample_rate: spec.sample_rates[0],
                        rate: 1.0,
                        instruction: None,
                        word_timestamps: false,
                    },
                    &tx,
                    None,
                    &token,
                ));
                drop(tx);
                let chunks = drain.join().unwrap_or(0);

                match outcome {
                    Ok(_) if chunks > 0 => eprintln!("  ok      {} {id} ({name})", spec.id),
                    Ok(_) => {
                        eprintln!("  SILENT  {} {id} ({name})", spec.id);
                        rejected.push(format!("{} {id}: no audio", spec.id));
                    }
                    Err(err) => {
                        eprintln!("  REJECT  {} {id} ({name}): {err}", spec.id);
                        rejected.push(format!("{} {id}: {err}", spec.id));
                    }
                }
            }
        }

        assert!(rejected.is_empty(), "voice table is wrong:\n{rejected:#?}");
    }
}
