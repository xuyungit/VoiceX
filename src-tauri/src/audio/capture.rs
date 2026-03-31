//! Audio capture service
//!
//! Captures microphone input, emits 100ms PCM chunks, and persists compressed
//! recordings to disk.

use std::{
    fs::File,
    io::{Seek, SeekFrom, Write},
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicBool, AtomicU64, Ordering},
        Arc, Mutex,
    },
    time::Instant,
};

use audiopus::{
    coder::Encoder as OpusEncoder, Application, Channels, SampleRate as OpusSampleRate,
};
use chrono::Utc;
use cpal::{
    traits::{DeviceTrait, StreamTrait},
    SampleFormat, SampleRate, StreamConfig,
};
use ogg::writing::{PacketWriteEndInfo, PacketWriter};
use rand::RngCore;
use serde::{Deserialize, Serialize};
use tokio::sync::mpsc::{self, Receiver, Sender};

use super::{device::AudioInputDeviceManager, AudioChunker};

/// Audio capture configuration
#[derive(Debug, Clone)]
pub struct AudioConfig {
    pub sample_rate: u32,
    pub channels: u16,
    pub chunk_ms: u32,
}

impl Default for AudioConfig {
    fn default() -> Self {
        Self {
            sample_rate: 16000,
            channels: 1,
            chunk_ms: 100,
        }
    }
}

/// Handle returned when capture starts
pub struct AudioCaptureHandle {
    pub receiver: Receiver<Vec<u8>>,
    pub level_receiver: Receiver<f32>,
    pub file_path: Option<PathBuf>,
    pub sample_rate: u32,
    pub channels: u16,
}

/// Summary returned when capture stops
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AudioRecordingResult {
    pub path: Option<PathBuf>,
    pub refinement_path: Option<PathBuf>,
    pub duration_ms: u64,
    pub bytes_written: u64,
    pub sample_rate: u32,
    pub channels: u16,
}

struct ActiveCapture {
    shared: Arc<CaptureShared>,
    stream: SendableStream,
    file_path: Option<PathBuf>,
    sample_rate: u32,
    channels: u16,
}

struct SendableStream(cpal::Stream);
unsafe impl Send for SendableStream {}
unsafe impl Sync for SendableStream {}

struct CaptureShared {
    tx: Mutex<Option<Sender<Vec<u8>>>>,
    level_tx: Mutex<Option<Sender<f32>>>,
    chunker: Mutex<AudioChunker>,
    sink: Mutex<Option<OggOpusSink>>,
    wav_sink: Mutex<Option<WavSink>>,
    refinement_pcm: Mutex<Option<Vec<u8>>>,
    sample_rate: u32,
    start_instant: Instant,
    first_frame_logged: AtomicBool,
    first_non_silent_logged: AtomicBool,
    samples_written: AtomicU64,
    running: AtomicBool,
}

impl CaptureShared {
    fn new(
        tx: Sender<Vec<u8>>,
        level_tx: Sender<f32>,
        chunker: AudioChunker,
        sink: Option<OggOpusSink>,
        wav_sink: Option<WavSink>,
        capture_refinement_pcm: bool,
        sample_rate: u32,
    ) -> Self {
        Self {
            tx: Mutex::new(Some(tx)),
            level_tx: Mutex::new(Some(level_tx)),
            chunker: Mutex::new(chunker),
            sink: Mutex::new(sink),
            wav_sink: Mutex::new(wav_sink),
            refinement_pcm: Mutex::new(if capture_refinement_pcm {
                Some(Vec::new())
            } else {
                None
            }),
            sample_rate,
            start_instant: Instant::now(),
            first_frame_logged: AtomicBool::new(false),
            first_non_silent_logged: AtomicBool::new(false),
            samples_written: AtomicU64::new(0),
            running: AtomicBool::new(true),
        }
    }

    fn dispatch(&self, data: &[u8]) {
        if !self.running.load(Ordering::SeqCst) {
            return;
        }

        if let Ok(mut chunker) = self.chunker.lock() {
            let chunks = chunker.append(data);
            drop(chunker);

            if let Ok(mut tx_guard) = self.tx.lock() {
                if let Some(tx) = tx_guard.as_mut() {
                    for chunk in chunks {
                        let _ = tx.try_send(chunk);
                    }
                }
            }
        }

        if let Ok(mut sink_guard) = self.sink.lock() {
            if let Some(sink) = sink_guard.as_mut() {
                let _ = sink.write(data);
            }
        }

        if let Ok(mut wav_guard) = self.wav_sink.lock() {
            if let Some(wav) = wav_guard.as_mut() {
                let _ = wav.write(data);
            }
        }

        if let Ok(mut refinement_guard) = self.refinement_pcm.lock() {
            if let Some(buffer) = refinement_guard.as_mut() {
                buffer.extend_from_slice(data);
            }
        }
    }

    fn dispatch_level(&self, level: f32) {
        if !self.running.load(Ordering::SeqCst) {
            return;
        }

        if let Ok(mut level_guard) = self.level_tx.lock() {
            if let Some(tx) = level_guard.as_mut() {
                let _ = tx.try_send(level.clamp(0.0, 1.0));
            }
        }
    }

    fn increment_samples(&self, frames: usize) {
        self.samples_written
            .fetch_add(frames as u64, Ordering::SeqCst);
    }

    fn flush_and_close(&self) -> (Option<OggOpusSink>, Option<WavSink>, Option<Vec<u8>>) {
        self.running.store(false, Ordering::SeqCst);

        // Flush remaining audio and send it BEFORE closing the channel,
        // so the ASR client receives the final audio segment.
        if let Ok(mut chunker) = self.chunker.lock() {
            if let Some(final_chunk) = chunker.flush() {
                if let Ok(tx_guard) = self.tx.lock() {
                    if let Some(tx) = tx_guard.as_ref() {
                        let _ = tx.try_send(final_chunk);
                    }
                }
            }
        }

        // Now drop sender so downstream consumers see channel closure.
        if let Ok(mut tx_guard) = self.tx.lock() {
            tx_guard.take();
        }
        if let Ok(mut level_guard) = self.level_tx.lock() {
            level_guard.take();
        }

        let sink = self.sink.lock().ok().and_then(|mut s| s.take());
        let wav_sink = self.wav_sink.lock().ok().and_then(|mut s| s.take());
        let refinement_pcm = self.refinement_pcm.lock().ok().and_then(|mut s| s.take());
        (sink, wav_sink, refinement_pcm)
    }

    fn samples_written(&self) -> u64 {
        self.samples_written.load(Ordering::SeqCst)
    }

    fn mark_first_frame_if_needed(&self) {
        if !self.first_frame_logged.swap(true, Ordering::SeqCst) {
            let elapsed = self.start_instant.elapsed();
            log::debug!(
                "First audio frame received after {:.2?} ({} Hz)",
                elapsed,
                self.sample_rate
            );
        }
    }
}

struct OggOpusSink {
    path: PathBuf,
    writer: PacketWriter<'static, File>,
    encoder: OpusEncoder,
    pcm_buffer: Vec<i16>,
    frame_samples: usize,
    input_sample_rate: u32,
    granule_pos: u64,
    stream_serial: u32,
    first_packet_logged: bool,
    start_instant: Instant,
    packet_count: u64,
}

struct WavSink {
    path: PathBuf,
    file: File,
    bytes_written: u32,
}

impl WavSink {
    fn create(dir: &Path, sample_rate: u32, channels: u16) -> Result<Self, AudioCaptureError> {
        std::fs::create_dir_all(dir).map_err(|e| {
            AudioCaptureError::StartFailed(format!("Failed to create recording dir: {}", e))
        })?;

        let filename = format!("recording-{}-raw.wav", Utc::now().format("%Y%m%d-%H%M%S"));
        let path = dir.join(filename);
        let mut file = File::create(&path).map_err(|e| {
            AudioCaptureError::StartFailed(format!("Failed to create wav file: {}", e))
        })?;

        write_wav_header(&mut file, sample_rate, channels)?;

        Ok(Self {
            path,
            file,
            bytes_written: 0,
        })
    }

    fn write(&mut self, data: &[u8]) -> Result<(), AudioCaptureError> {
        self.file.write_all(data).map_err(|e| {
            AudioCaptureError::StartFailed(format!("Failed to write wav data: {}", e))
        })?;
        self.bytes_written = self.bytes_written.saturating_add(data.len() as u32);
        Ok(())
    }

    fn finish(mut self) -> Result<(PathBuf, u32), AudioCaptureError> {
        let data_size = self.bytes_written;
        let riff_size = 36u32.saturating_add(data_size);

        self.file.seek(SeekFrom::Start(4)).map_err(|e| {
            AudioCaptureError::StopFailed(format!("Failed to seek wav header: {}", e))
        })?;
        self.file.write_all(&riff_size.to_le_bytes()).map_err(|e| {
            AudioCaptureError::StopFailed(format!("Failed to write wav size: {}", e))
        })?;
        self.file.seek(SeekFrom::Start(40)).map_err(|e| {
            AudioCaptureError::StopFailed(format!("Failed to seek wav data size: {}", e))
        })?;
        self.file.write_all(&data_size.to_le_bytes()).map_err(|e| {
            AudioCaptureError::StopFailed(format!("Failed to write wav data size: {}", e))
        })?;

        Ok((self.path, data_size))
    }
}

impl OggOpusSink {
    fn create(dir: &Path, sample_rate: u32) -> Result<Self, AudioCaptureError> {
        std::fs::create_dir_all(dir).map_err(|e| {
            AudioCaptureError::StartFailed(format!("Failed to create recording dir: {}", e))
        })?;

        let filename = format!("recording-{}.opus", Utc::now().format("%Y%m%d-%H%M%S"));
        let path = dir.join(filename);
        let file = File::create(&path)
            .map_err(|e| AudioCaptureError::StartFailed(format!("Failed to create file: {}", e)))?;

        let opus_rate = match sample_rate {
            8000 => OpusSampleRate::Hz8000,
            12000 => OpusSampleRate::Hz12000,
            16000 => OpusSampleRate::Hz16000,
            24000 => OpusSampleRate::Hz24000,
            _ => OpusSampleRate::Hz48000,
        };

        let encoder =
            OpusEncoder::new(opus_rate, Channels::Mono, Application::Audio).map_err(|e| {
                AudioCaptureError::StartFailed(format!("Failed to init Opus encoder: {}", e))
            })?;
        let lookahead = encoder.lookahead().unwrap_or(0);
        let pre_skip =
            ((lookahead as u64) * 48000 / sample_rate as u64).min(u16::MAX as u64) as u16;

        let mut writer: PacketWriter<'static, File> = PacketWriter::new(file);
        let stream_serial = rand::thread_rng().next_u32();

        // OpusHead
        let mut opus_head = Vec::with_capacity(19);
        opus_head.extend_from_slice(b"OpusHead");
        opus_head.push(1); // version
        opus_head.push(1); // channels
        opus_head.extend_from_slice(&pre_skip.to_le_bytes());
        opus_head.extend_from_slice(&sample_rate.to_le_bytes()); // input sample rate
        opus_head.extend_from_slice(&0i16.to_le_bytes()); // output gain
        opus_head.push(0); // channel mapping
        writer
            .write_packet(opus_head, stream_serial, PacketWriteEndInfo::EndPage, 0)
            .map_err(|e| {
                AudioCaptureError::StartFailed(format!("Failed to write OpusHead: {}", e))
            })?;

        // OpusTags (vendor only)
        let vendor = b"VoiceX";
        let mut tags = Vec::with_capacity(8 + vendor.len() + 4);
        tags.extend_from_slice(b"OpusTags");
        tags.extend_from_slice(&(vendor.len() as u32).to_le_bytes());
        tags.extend_from_slice(vendor);
        tags.extend_from_slice(&0u32.to_le_bytes()); // user comment list length
        writer
            .write_packet(tags, stream_serial, PacketWriteEndInfo::EndPage, 0)
            .map_err(|e| {
                AudioCaptureError::StartFailed(format!("Failed to write OpusTags: {}", e))
            })?;

        Ok(Self {
            path,
            writer,
            encoder,
            pcm_buffer: Vec::with_capacity(960),
            frame_samples: (sample_rate as usize / 50).max(1), // 20ms frames
            input_sample_rate: sample_rate,
            granule_pos: 0,
            stream_serial,
            first_packet_logged: false,
            start_instant: Instant::now(),
            packet_count: 0,
        })
    }

    fn write(&mut self, data: &[u8]) -> Result<(), AudioCaptureError> {
        if data.len() % 2 != 0 {
            return Err(AudioCaptureError::StartFailed(
                "PCM chunk not aligned to i16".into(),
            ));
        }

        for chunk in data.chunks_exact(2) {
            self.pcm_buffer
                .push(i16::from_le_bytes([chunk[0], chunk[1]]));
        }

        while self.pcm_buffer.len() >= self.frame_samples {
            let frame: Vec<i16> = self.pcm_buffer.drain(..self.frame_samples).collect();
            self.encode_and_write(&frame)?;
        }

        Ok(())
    }

    fn encode_and_write(&mut self, frame: &[i16]) -> Result<(), AudioCaptureError> {
        let mut output = vec![0u8; 4000];
        let len = self
            .encoder
            .encode(frame, &mut output)
            .map_err(|e| AudioCaptureError::StartFailed(format!("Opus encode failed: {}", e)))?;
        output.truncate(len);

        // Opus granule position counts 48k samples; scale from 16k input.
        let frame_samples_48k = (frame.len() as u64) * 48000 / (self.input_sample_rate as u64);
        let next_granule = self.granule_pos + frame_samples_48k;

        let end_info = if self.packet_count % 20 == 19 {
            PacketWriteEndInfo::EndPage
        } else {
            PacketWriteEndInfo::NormalPacket
        };

        self.writer
            .write_packet(output, self.stream_serial, end_info, next_granule)
            .map_err(|e| {
                AudioCaptureError::StartFailed(format!("Failed to write Opus packet: {}", e))
            })?;

        if !self.first_packet_logged {
            self.first_packet_logged = true;
            let elapsed = self.start_instant.elapsed();
            log::debug!("First Opus packet written after {:.2?}", elapsed);
        }
        self.packet_count += 1;
        self.granule_pos = next_granule;
        Ok(())
    }

    fn finish(mut self) -> Result<(PathBuf, u64), AudioCaptureError> {
        if !self.pcm_buffer.is_empty() {
            let mut frame = self.pcm_buffer.clone();
            frame.resize(self.frame_samples, 0);
            self.encode_and_write(&frame)?;
            self.pcm_buffer.clear();
        }

        self.writer
            .write_packet(
                Vec::new(),
                self.stream_serial,
                PacketWriteEndInfo::EndStream,
                self.granule_pos,
            )
            .map_err(|e| {
                AudioCaptureError::StopFailed(format!("Failed to finalize opus stream: {}", e))
            })?;

        let file = self.writer.into_inner();

        let bytes = file
            .metadata()
            .map_err(|e| AudioCaptureError::StopFailed(format!("Failed to stat opus file: {}", e)))?
            .len();

        Ok((self.path, bytes))
    }
}

/// Audio capture service
pub struct AudioCaptureService {
    config: AudioConfig,
    preferred_device_uid: Option<String>,
    active: Option<ActiveCapture>,
}

impl AudioCaptureService {
    pub fn new() -> Self {
        Self {
            config: AudioConfig::default(),
            preferred_device_uid: None,
            active: None,
        }
    }

    pub fn set_preferred_device(&mut self, uid: Option<String>) {
        self.preferred_device_uid = uid;
    }

    /// Start audio capture (emits mono PCM i16 chunks at the device sample rate).
    pub fn start(
        &mut self,
        recordings_dir: Option<&Path>,
        capture_refinement_pcm: bool,
    ) -> Result<AudioCaptureHandle, AudioCaptureError> {
        if self.active.is_some() {
            return Err(AudioCaptureError::AlreadyRunning);
        }

        let device = AudioInputDeviceManager::resolve_device(self.preferred_device_uid.as_deref())?;
        let output_channels = self.config.channels;
        let (config, sample_format) = Self::select_config(&device, output_channels)?;
        let input_channels = config.channels;
        let device_name = device.name().unwrap_or_else(|_| "Unknown device".into());

        let chunk_bytes = bytes_per_ms(config.sample_rate.0, self.config.chunk_ms);
        let chunker = AudioChunker::new(chunk_bytes);

        let file_sink = if let Some(dir) = recordings_dir {
            Some(OggOpusSink::create(dir, config.sample_rate.0)?)
        } else {
            None
        };

        let wav_sink = if let Some(dir) = recordings_dir {
            if is_debug_wav_enabled() {
                match WavSink::create(dir, config.sample_rate.0, output_channels) {
                    Ok(sink) => {
                        log::info!("Debug WAV recording enabled: {}", sink.path.display());
                        Some(sink)
                    }
                    Err(err) => {
                        log::error!("Failed to create debug WAV sink: {}", err);
                        None
                    }
                }
            } else {
                None
            }
        } else {
            None
        };

        let (tx, rx) = mpsc::channel(32);
        let (level_tx, level_rx) = mpsc::channel(64);
        let shared = Arc::new(CaptureShared::new(
            tx,
            level_tx,
            chunker,
            file_sink,
            wav_sink,
            capture_refinement_pcm,
            config.sample_rate.0,
        ));

        let stream = Self::build_stream(device, config.clone(), sample_format, shared.clone())?;
        stream.0.play().map_err(|e| {
            AudioCaptureError::StartFailed(format!("Failed to start stream: {}", e))
        })?;

        let file_path = shared
            .sink
            .lock()
            .ok()
            .and_then(|sink| sink.as_ref().map(|s| s.path.clone()));

        self.active = Some(ActiveCapture {
            shared,
            stream,
            file_path: file_path.clone(),
            sample_rate: config.sample_rate.0,
            channels: output_channels,
        });

        log::debug!(
            "Audio capture started at {} Hz ({} ch in -> {} ch out) on device '{}'",
            config.sample_rate.0,
            input_channels,
            output_channels,
            device_name
        );

        Ok(AudioCaptureHandle {
            receiver: rx,
            level_receiver: level_rx,
            file_path,
            sample_rate: config.sample_rate.0,
            channels: output_channels,
        })
    }

    /// Stop audio capture and finalize the recording.
    pub fn stop(&mut self) -> Result<AudioRecordingResult, AudioCaptureError> {
        let active = self.active.take().ok_or(AudioCaptureError::NotRunning)?;
        let ActiveCapture {
            shared,
            stream,
            file_path,
            sample_rate,
            channels,
        } = active;

        // Pause the stream before dropping to ensure proper device release on macOS
        if let Err(e) = stream.0.pause() {
            log::warn!("Failed to pause audio stream: {}", e);
        }
        drop(stream);
        let (sink, wav_sink, refinement_pcm) = shared.flush_and_close();

        let samples = shared.samples_written();
        let duration_ms = if sample_rate > 0 {
            samples.saturating_mul(1000) / (sample_rate as u64)
        } else {
            0
        };

        let mut bytes_written = 0;
        let mut path = file_path;
        let mut refinement_path = None;

        if let Some(sink) = sink {
            match sink.finish() {
                Ok((p, bytes)) => {
                    path = Some(p);
                    bytes_written = bytes;
                }
                Err(err) => {
                    log::error!("Failed to finalize audio file: {}", err);
                }
            }
        }

        if let Some(wav_sink) = wav_sink {
            match wav_sink.finish() {
                Ok((p, bytes)) => {
                    log::info!("Debug WAV saved ({} bytes): {}", bytes, p.display());
                }
                Err(err) => {
                    log::error!("Failed to finalize debug WAV: {}", err);
                }
            }
        }

        if let Some(pcm) = refinement_pcm {
            if !pcm.is_empty() {
                match write_temp_wav(sample_rate, channels, &pcm) {
                    Ok(path) => {
                        refinement_path = Some(path);
                    }
                    Err(err) => {
                        log::error!("Failed to create refinement WAV: {}", err);
                    }
                }
            }
        }

        log::debug!(
            "Audio capture stopped (duration {} ms, bytes {})",
            duration_ms,
            bytes_written
        );

        Ok(AudioRecordingResult {
            path,
            refinement_path,
            duration_ms,
            bytes_written,
            sample_rate,
            channels,
        })
    }

    pub fn is_running(&self) -> bool {
        self.active.is_some()
    }

    fn select_config(
        device: &cpal::Device,
        channels: u16,
    ) -> Result<(StreamConfig, SampleFormat), AudioCaptureError> {
        let configs: Vec<_> = device
            .supported_input_configs()
            .map_err(|e| AudioCaptureError::StartFailed(format!("Failed to query configs: {}", e)))?
            .collect();

        let default_config = device
            .default_input_config()
            .map_err(|_| AudioCaptureError::NoDevice)?;

        let config_for_rate = |range: &cpal::SupportedStreamConfigRange, rate: u32| {
            range.try_with_sample_rate(SampleRate(rate)).map(|_| {
                (
                    StreamConfig {
                        channels: range.channels(),
                        sample_rate: SampleRate(rate),
                        buffer_size: cpal::BufferSize::Default,
                    },
                    range.sample_format(),
                )
            })
        };

        #[cfg(target_os = "windows")]
        {
            let default_rate = default_config.sample_rate().0;
            let default_format = default_config.sample_format();

            // Prefer the device default on Windows to avoid low-quality resampling paths.
            for range in configs.iter() {
                if range.sample_format() == default_format && range.channels() == channels {
                    if let Some(config) = config_for_rate(range, default_rate) {
                        return Ok(config);
                    }
                }
            }

            for range in configs.iter() {
                if range.sample_format() == default_format {
                    if let Some(config) = config_for_rate(range, default_rate) {
                        return Ok(config);
                    }
                }
            }

            for range in configs.iter() {
                if range.channels() == channels {
                    if let Some(config) = config_for_rate(range, default_rate) {
                        return Ok(config);
                    }
                }
            }

            for range in configs.iter() {
                if let Some(config) = config_for_rate(range, default_rate) {
                    return Ok(config);
                }
            }
        }

        // Try preferred Opus-friendly sample rates in order.
        for &target in &[16000u32, 48000u32, 24000u32] {
            for range in configs.iter() {
                if range.channels() == channels {
                    if let Some(config) = config_for_rate(range, target) {
                        return Ok(config);
                    }
                }
            }

            for range in configs.iter() {
                if let Some(config) = config_for_rate(range, target) {
                    return Ok(config);
                }
            }
        }

        Ok((default_config.config(), default_config.sample_format()))
    }

    fn build_stream(
        device: cpal::Device,
        config: StreamConfig,
        sample_format: SampleFormat,
        shared: Arc<CaptureShared>,
    ) -> Result<SendableStream, AudioCaptureError> {
        match sample_format {
            SampleFormat::I8 => Self::build_stream_i8(device, config, shared),
            SampleFormat::U8 => Self::build_stream_u8(device, config, shared),
            SampleFormat::I16 => Self::build_stream_i16(device, config, shared),
            SampleFormat::U16 => Self::build_stream_u16(device, config, shared),
            SampleFormat::I32 => Self::build_stream_i32(device, config, shared),
            SampleFormat::U32 => Self::build_stream_u32(device, config, shared),
            SampleFormat::I64 => Self::build_stream_i64(device, config, shared),
            SampleFormat::U64 => Self::build_stream_u64(device, config, shared),
            SampleFormat::F32 => Self::build_stream_f32(device, config, shared),
            SampleFormat::F64 => Self::build_stream_f64(device, config, shared),
            other => Err(AudioCaptureError::StartFailed(format!(
                "Unsupported sample format: {:?}",
                other
            ))),
        }
    }

    fn build_stream_i8(
        device: cpal::Device,
        config: StreamConfig,
        shared: Arc<CaptureShared>,
    ) -> Result<SendableStream, AudioCaptureError> {
        let channels = config.channels as usize;
        let stream = device
            .build_input_stream(
                &config,
                move |data: &[i8], _: &cpal::InputCallbackInfo| {
                    process_input(data, channels, &shared, |s| *s as f32 / i8::MAX as f32);
                },
                move |err| {
                    log::error!("Audio input error: {}", err);
                },
                None,
            )
            .map_err(|e| {
                AudioCaptureError::StartFailed(format!("Failed to build stream: {}", e))
            })?;

        Ok(SendableStream(stream))
    }

    fn build_stream_u8(
        device: cpal::Device,
        config: StreamConfig,
        shared: Arc<CaptureShared>,
    ) -> Result<SendableStream, AudioCaptureError> {
        let channels = config.channels as usize;
        let stream = device
            .build_input_stream(
                &config,
                move |data: &[u8], _: &cpal::InputCallbackInfo| {
                    process_input(data, channels, &shared, |s| {
                        // Convert 0..255 to -1.0..1.0
                        (*s as f32 / u8::MAX as f32) * 2.0 - 1.0
                    });
                },
                move |err| {
                    log::error!("Audio input error: {}", err);
                },
                None,
            )
            .map_err(|e| {
                AudioCaptureError::StartFailed(format!("Failed to build stream: {}", e))
            })?;

        Ok(SendableStream(stream))
    }

    fn build_stream_i16(
        device: cpal::Device,
        config: StreamConfig,
        shared: Arc<CaptureShared>,
    ) -> Result<SendableStream, AudioCaptureError> {
        let channels = config.channels as usize;

        let stream = device
            .build_input_stream(
                &config,
                move |data: &[i16], _: &cpal::InputCallbackInfo| {
                    process_input(data, channels, &shared, |s| *s as f32 / i16::MAX as f32);
                },
                move |err| {
                    log::error!("Audio input error: {}", err);
                },
                None,
            )
            .map_err(|e| {
                AudioCaptureError::StartFailed(format!("Failed to build stream: {}", e))
            })?;

        Ok(SendableStream(stream))
    }

    fn build_stream_u16(
        device: cpal::Device,
        config: StreamConfig,
        shared: Arc<CaptureShared>,
    ) -> Result<SendableStream, AudioCaptureError> {
        let channels = config.channels as usize;
        let stream = device
            .build_input_stream(
                &config,
                move |data: &[u16], _: &cpal::InputCallbackInfo| {
                    process_input(data, channels, &shared, |s| {
                        // Convert 0..65535 to -1.0..1.0
                        (*s as f32 / u16::MAX as f32) * 2.0 - 1.0
                    });
                },
                move |err| {
                    log::error!("Audio input error: {}", err);
                },
                None,
            )
            .map_err(|e| {
                AudioCaptureError::StartFailed(format!("Failed to build stream: {}", e))
            })?;

        Ok(SendableStream(stream))
    }

    fn build_stream_i32(
        device: cpal::Device,
        config: StreamConfig,
        shared: Arc<CaptureShared>,
    ) -> Result<SendableStream, AudioCaptureError> {
        let channels = config.channels as usize;
        let stream = device
            .build_input_stream(
                &config,
                move |data: &[i32], _: &cpal::InputCallbackInfo| {
                    process_input(data, channels, &shared, |s| *s as f32 / i32::MAX as f32);
                },
                move |err| {
                    log::error!("Audio input error: {}", err);
                },
                None,
            )
            .map_err(|e| {
                AudioCaptureError::StartFailed(format!("Failed to build stream: {}", e))
            })?;

        Ok(SendableStream(stream))
    }

    fn build_stream_u32(
        device: cpal::Device,
        config: StreamConfig,
        shared: Arc<CaptureShared>,
    ) -> Result<SendableStream, AudioCaptureError> {
        let channels = config.channels as usize;
        let stream = device
            .build_input_stream(
                &config,
                move |data: &[u32], _: &cpal::InputCallbackInfo| {
                    process_input(data, channels, &shared, |s| {
                        (*s as f32 / u32::MAX as f32) * 2.0 - 1.0
                    });
                },
                move |err| {
                    log::error!("Audio input error: {}", err);
                },
                None,
            )
            .map_err(|e| {
                AudioCaptureError::StartFailed(format!("Failed to build stream: {}", e))
            })?;

        Ok(SendableStream(stream))
    }

    fn build_stream_i64(
        device: cpal::Device,
        config: StreamConfig,
        shared: Arc<CaptureShared>,
    ) -> Result<SendableStream, AudioCaptureError> {
        let channels = config.channels as usize;
        let stream = device
            .build_input_stream(
                &config,
                move |data: &[i64], _: &cpal::InputCallbackInfo| {
                    process_input(data, channels, &shared, |s| *s as f32 / i64::MAX as f32);
                },
                move |err| {
                    log::error!("Audio input error: {}", err);
                },
                None,
            )
            .map_err(|e| {
                AudioCaptureError::StartFailed(format!("Failed to build stream: {}", e))
            })?;

        Ok(SendableStream(stream))
    }

    fn build_stream_u64(
        device: cpal::Device,
        config: StreamConfig,
        shared: Arc<CaptureShared>,
    ) -> Result<SendableStream, AudioCaptureError> {
        let channels = config.channels as usize;
        let stream = device
            .build_input_stream(
                &config,
                move |data: &[u64], _: &cpal::InputCallbackInfo| {
                    process_input(data, channels, &shared, |s| {
                        (*s as f32 / u64::MAX as f32) * 2.0 - 1.0
                    });
                },
                move |err| {
                    log::error!("Audio input error: {}", err);
                },
                None,
            )
            .map_err(|e| {
                AudioCaptureError::StartFailed(format!("Failed to build stream: {}", e))
            })?;

        Ok(SendableStream(stream))
    }

    fn build_stream_f32(
        device: cpal::Device,
        config: StreamConfig,
        shared: Arc<CaptureShared>,
    ) -> Result<SendableStream, AudioCaptureError> {
        let channels = config.channels as usize;
        let stream = device
            .build_input_stream(
                &config,
                move |data: &[f32], _: &cpal::InputCallbackInfo| {
                    process_input(data, channels, &shared, |s| *s);
                },
                move |err| {
                    log::error!("Audio input error: {}", err);
                },
                None,
            )
            .map_err(|e| {
                AudioCaptureError::StartFailed(format!("Failed to build stream: {}", e))
            })?;

        Ok(SendableStream(stream))
    }

    fn build_stream_f64(
        device: cpal::Device,
        config: StreamConfig,
        shared: Arc<CaptureShared>,
    ) -> Result<SendableStream, AudioCaptureError> {
        let channels = config.channels as usize;
        let stream = device
            .build_input_stream(
                &config,
                move |data: &[f64], _: &cpal::InputCallbackInfo| {
                    process_input(data, channels, &shared, |s| *s as f32);
                },
                move |err| {
                    log::error!("Audio input error: {}", err);
                },
                None,
            )
            .map_err(|e| {
                AudioCaptureError::StartFailed(format!("Failed to build stream: {}", e))
            })?;

        Ok(SendableStream(stream))
    }
}

fn process_input<T, F>(data: &[T], channels: usize, shared: &Arc<CaptureShared>, to_f32: F)
where
    F: Fn(&T) -> f32,
{
    shared.mark_first_frame_if_needed();

    let frames = data.len() / channels;
    if frames == 0 {
        return;
    }

    let mut mono = Vec::with_capacity(frames);
    for frame in data.chunks_exact(channels) {
        let mut sum = 0f32;
        for sample in frame {
            sum += to_f32(sample);
        }
        let avg = sum / channels as f32;
        let clamped = (avg * i16::MAX as f32).clamp(i16::MIN as f32, i16::MAX as f32) as i16;
        mono.push(clamped);
    }

    let rms = mono
        .iter()
        .map(|s| (*s as f32 / i16::MAX as f32).powi(2))
        .sum::<f32>()
        .sqrt()
        / (mono.len() as f32).sqrt();

    shared.dispatch_level(rms);

    // Detect first non-silent audio (~-35dB threshold)
    if !shared.first_non_silent_logged.load(Ordering::SeqCst) && rms > 0.0175 {
        if !shared.first_non_silent_logged.swap(true, Ordering::SeqCst) {
            let elapsed = shared.start_instant.elapsed();
            log::debug!(
                "First non-silent audio after {:.2?} (rms {:.4})",
                elapsed,
                rms
            );
        }
    }

    shared.increment_samples(frames);

    let mut bytes = Vec::with_capacity(mono.len() * 2);
    for sample in mono {
        bytes.extend_from_slice(&sample.to_le_bytes());
    }

    shared.dispatch(&bytes);
}

impl Default for AudioCaptureService {
    fn default() -> Self {
        Self::new()
    }
}

fn bytes_per_ms(sample_rate: u32, chunk_ms: u32) -> usize {
    // mono i16 -> 2 bytes per sample
    let bytes_per_ms = (sample_rate as usize * 2) / 1000;
    bytes_per_ms * chunk_ms as usize
}

fn is_debug_wav_enabled() -> bool {
    std::env::var("VOICEX_DEBUG_WAV")
        .map(|value| {
            let value = value.trim().to_ascii_lowercase();
            value == "1" || value == "true" || value == "yes"
        })
        .unwrap_or(false)
}

fn write_temp_wav(
    sample_rate: u32,
    channels: u16,
    pcm_bytes: &[u8],
) -> Result<PathBuf, AudioCaptureError> {
    let dir = std::env::temp_dir().join("voicex-refinement");
    std::fs::create_dir_all(&dir).map_err(|e| {
        AudioCaptureError::StopFailed(format!("Failed to create refinement temp dir: {}", e))
    })?;

    let filename = format!(
        "refinement-{}-{}.wav",
        Utc::now().format("%Y%m%d-%H%M%S"),
        rand::random::<u32>()
    );
    let path = dir.join(filename);
    let mut file = File::create(&path).map_err(|e| {
        AudioCaptureError::StopFailed(format!("Failed to create refinement wav file: {}", e))
    })?;
    write_wav_header(&mut file, sample_rate, channels)?;
    file.write_all(pcm_bytes).map_err(|e| {
        AudioCaptureError::StopFailed(format!("Failed to write refinement wav data: {}", e))
    })?;
    file.seek(SeekFrom::Start(4)).map_err(|e| {
        AudioCaptureError::StopFailed(format!("Failed to seek refinement wav header: {}", e))
    })?;
    let riff_size = 36u32.saturating_add(pcm_bytes.len() as u32);
    file.write_all(&riff_size.to_le_bytes()).map_err(|e| {
        AudioCaptureError::StopFailed(format!("Failed to write refinement wav size: {}", e))
    })?;
    file.seek(SeekFrom::Start(40)).map_err(|e| {
        AudioCaptureError::StopFailed(format!("Failed to seek refinement wav data size: {}", e))
    })?;
    file.write_all(&(pcm_bytes.len() as u32).to_le_bytes())
        .map_err(|e| {
            AudioCaptureError::StopFailed(format!(
                "Failed to write refinement wav data size: {}",
                e
            ))
        })?;

    Ok(path)
}

fn write_wav_header(
    file: &mut File,
    sample_rate: u32,
    channels: u16,
) -> Result<(), AudioCaptureError> {
    let byte_rate = sample_rate * channels as u32 * 2;
    let block_align = channels * 2;

    let mut header = Vec::with_capacity(44);
    header.extend_from_slice(b"RIFF");
    header.extend_from_slice(&0u32.to_le_bytes()); // chunk size placeholder
    header.extend_from_slice(b"WAVE");
    header.extend_from_slice(b"fmt ");
    header.extend_from_slice(&16u32.to_le_bytes()); // PCM header length
    header.extend_from_slice(&1u16.to_le_bytes()); // PCM format
    header.extend_from_slice(&channels.to_le_bytes());
    header.extend_from_slice(&sample_rate.to_le_bytes());
    header.extend_from_slice(&byte_rate.to_le_bytes());
    header.extend_from_slice(&block_align.to_le_bytes());
    header.extend_from_slice(&16u16.to_le_bytes()); // bits per sample
    header.extend_from_slice(b"data");
    header.extend_from_slice(&0u32.to_le_bytes()); // data size placeholder

    file.write_all(&header).map_err(|e| {
        AudioCaptureError::StartFailed(format!("Failed to write wav header: {}", e))
    })?;
    Ok(())
}

#[derive(Debug, thiserror::Error)]
pub enum AudioCaptureError {
    #[error("Audio capture is already running")]
    AlreadyRunning,

    #[error("Audio capture is not running")]
    NotRunning,

    #[error("No input device available")]
    NoDevice,

    #[error("Failed to start capture: {0}")]
    StartFailed(String),

    #[error("Failed to stop capture: {0}")]
    StopFailed(String),
}
