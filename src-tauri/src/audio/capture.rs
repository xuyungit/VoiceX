//! Audio capture service
//!
//! Captures microphone input, emits 100ms PCM chunks, and persists compressed
//! recordings to disk.

use std::{
    f32::consts::TAU,
    fs::File,
    io::{Seek, SeekFrom, Write},
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering},
        mpsc as std_mpsc, Arc,
    },
    thread::JoinHandle,
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

use super::{device::AudioInputDeviceManager, resample::LinearResampler, AudioChunker};

const AUDIO_VIZ_BAND_EDGES: [f32; 9] = [
    120.0, 220.0, 400.0, 700.0, 1200.0, 2000.0, 3200.0, 4800.0, 7200.0,
];

/// Bounded frame backlog between the real-time cpal audio callback and the
/// background worker thread. Absorbs several seconds of OS disk/scheduling
/// jitter without ever blocking the audio callback thread.
const AUDIO_CALLBACK_QUEUE_CAPACITY: usize = 256;

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
    pub viz_receiver: Receiver<AudioVisualizationFrame>,
    pub file_path: Option<PathBuf>,
    pub sample_rate: u32,
    pub channels: u16,
}

#[derive(Debug, Clone)]
pub struct AudioVisualizationFrame {
    pub level: f32,
    pub bands: Vec<f32>,
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

enum WorkerCommand {
    Frame(Vec<i16>),
    Stop,
}

struct ActiveCapture {
    stream: Option<SendableStream>,
    callback_state: Arc<CallbackState>,
    stop_tx: std_mpsc::Sender<WorkerCommand>,
    worker: Option<JoinHandle<WorkerOutput>>,
    sample_rate: u32,
    channels: u16,
}

impl Drop for ActiveCapture {
    fn drop(&mut self) {
        self.callback_state.running.store(false, Ordering::SeqCst);
        if let Some(stream) = self.stream.take() {
            let _ = stream.0.pause();
            drop(stream);
        }
        let _ = self.stop_tx.send(WorkerCommand::Stop);
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}

struct SendableStream(cpal::Stream);
unsafe impl Send for SendableStream {}
unsafe impl Sync for SendableStream {}

/// Lock-free state shared with the real-time cpal input callback.
struct CallbackState {
    sample_rate: u32,
    start_instant: Instant,
    first_frame_logged: AtomicBool,
    queued_frames: AtomicUsize,
    dropped_callbacks: AtomicU64,
    running: AtomicBool,
    queue_capacity: usize,
}

impl CallbackState {
    fn new(sample_rate: u32) -> Self {
        Self::with_capacity(sample_rate, AUDIO_CALLBACK_QUEUE_CAPACITY)
    }

    fn with_capacity(sample_rate: u32, queue_capacity: usize) -> Self {
        Self {
            sample_rate,
            start_instant: Instant::now(),
            first_frame_logged: AtomicBool::new(false),
            queued_frames: AtomicUsize::new(0),
            dropped_callbacks: AtomicU64::new(0),
            running: AtomicBool::new(true),
            queue_capacity,
        }
    }

    fn is_running(&self) -> bool {
        self.running.load(Ordering::Relaxed)
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

    fn dropped_callbacks(&self) -> u64 {
        self.dropped_callbacks.load(Ordering::Relaxed)
    }
}

/// Owned by the cpal callback closure. Shutdown NEVER depends on cpal dropping
/// this closure (cpal 0.15.3 on macOS has an Arc cycle in its non-default
/// device disconnect listener that retains StreamInner); `stop()` instead
/// sends an explicit `WorkerCommand::Stop` sentinel.
struct CallbackProducer {
    raw_tx: std_mpsc::Sender<WorkerCommand>,
    state: Arc<CallbackState>,
}

impl CallbackProducer {
    fn enqueue_mono(&self, mono: Vec<i16>) {
        let prev = self.state.queued_frames.fetch_add(1, Ordering::AcqRel);
        if prev >= self.state.queue_capacity {
            self.state.queued_frames.fetch_sub(1, Ordering::AcqRel);
            self.state.dropped_callbacks.fetch_add(1, Ordering::Relaxed);
            return;
        }
        if self.raw_tx.send(WorkerCommand::Frame(mono)).is_err() {
            self.state.queued_frames.fetch_sub(1, Ordering::AcqRel);
        }
    }
}

struct WorkerOutput {
    sink: Option<OggOpusSink>,
    wav_sink: Option<WavSink>,
    refinement_pcm: Option<Vec<u8>>,
    samples_written: u64,
}

/// Dedicated background worker that owns visualization, chunking, Opus
/// encoding, and disk persistence off the real-time audio callback thread.
struct AudioCaptureWorker {
    tx: Sender<Vec<u8>>,
    viz_tx: Sender<AudioVisualizationFrame>,
    chunker: AudioChunker,
    sink: Option<OggOpusSink>,
    wav_sink: Option<WavSink>,
    refinement_pcm: Option<Vec<u8>>,
    pcm_scratch: Vec<u8>,
    sample_rate: u32,
    samples_written: u64,
}

impl AudioCaptureWorker {
    fn new(
        tx: Sender<Vec<u8>>,
        viz_tx: Sender<AudioVisualizationFrame>,
        chunker: AudioChunker,
        sink: Option<OggOpusSink>,
        wav_sink: Option<WavSink>,
        capture_refinement_pcm: bool,
        sample_rate: u32,
    ) -> Self {
        Self {
            tx,
            viz_tx,
            chunker,
            sink,
            wav_sink,
            refinement_pcm: capture_refinement_pcm.then(Vec::new),
            pcm_scratch: Vec::new(),
            sample_rate,
            samples_written: 0,
        }
    }

    fn run(
        mut self,
        raw_rx: std_mpsc::Receiver<WorkerCommand>,
        callback_state: Arc<CallbackState>,
    ) -> WorkerOutput {
        while let Ok(command) = raw_rx.recv() {
            match command {
                WorkerCommand::Frame(mono) => {
                    callback_state.queued_frames.fetch_sub(1, Ordering::AcqRel);
                    self.process_frame(&mono);
                }
                WorkerCommand::Stop => break,
            }
        }

        // Flush remaining audio and send it BEFORE dropping `tx`,
        // so the ASR client receives the final audio segment.
        if let Some(final_chunk) = self.chunker.flush() {
            let _ = self.tx.try_send(final_chunk);
        }
        drop(self.tx);
        drop(self.viz_tx);

        WorkerOutput {
            sink: self.sink,
            wav_sink: self.wav_sink,
            refinement_pcm: self.refinement_pcm,
            samples_written: self.samples_written,
        }
    }

    fn process_frame(&mut self, mono: &[i16]) {
        if mono.is_empty() {
            return;
        }

        let rms = mono
            .iter()
            .map(|s| (*s as f32 / i16::MAX as f32).powi(2))
            .sum::<f32>()
            .sqrt()
            / (mono.len() as f32).sqrt();
        let bands = compute_visualization_bands(mono, self.sample_rate, rms);
        let _ = self.viz_tx.try_send(AudioVisualizationFrame {
            level: rms.clamp(0.0, 1.0),
            bands: bands.to_vec(),
        });

        self.samples_written = self.samples_written.saturating_add(mono.len() as u64);

        self.pcm_scratch.clear();
        self.pcm_scratch.reserve(mono.len() * 2);
        for sample in mono {
            self.pcm_scratch.extend_from_slice(&sample.to_le_bytes());
        }

        for chunk in self.chunker.append(&self.pcm_scratch) {
            let _ = self.tx.try_send(chunk);
        }

        if let Some(sink) = self.sink.as_mut() {
            if let Err(error) = sink.write(mono) {
                let path = sink.path.clone();
                log::error!("Failed to write recording {}: {}", path.display(), error);
                drop(self.sink.take());
                remove_incomplete_recording(&path);
            }
        }

        if let Some(wav) = self.wav_sink.as_mut() {
            let _ = wav.write(&self.pcm_scratch);
        }

        if let Some(buffer) = self.refinement_pcm.as_mut() {
            buffer.extend_from_slice(&self.pcm_scratch);
        }
    }
}

fn cleanup_unused_sinks(file_sink: Option<OggOpusSink>, wav_sink: Option<WavSink>) {
    if let Some(sink) = file_sink {
        let path = sink.path.clone();
        drop(sink);
        remove_incomplete_recording(&path);
    }
    if let Some(wav) = wav_sink {
        let path = wav.path.clone();
        drop(wav);
        remove_incomplete_recording(&path);
    }
}

struct OggOpusSink {
    path: PathBuf,
    writer: PacketWriter<'static, File>,
    encoder: OpusEncoder,
    resampler: Option<LinearResampler>,
    pcm_buffer: Vec<i16>,
    frame_samples: usize,
    encoder_sample_rate: u32,
    granule_pos: u64,
    stream_serial: u32,
    first_packet_logged: bool,
    start_instant: Instant,
    packet_count: u64,
    // Keep the last encoded packet until stop so it can carry the EOS flag.
    pending_packet: Option<(Vec<u8>, u64, PacketWriteEndInfo)>,
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

        let filename = format!("recording-{}.opus", Utc::now().format("%Y%m%d-%H%M%S%.3f"));
        let path = dir.join(filename);
        let file = File::create_new(&path)
            .map_err(|e| AudioCaptureError::StartFailed(format!("Failed to create file: {}", e)))?;

        let opus_rate = match sample_rate {
            8000 => OpusSampleRate::Hz8000,
            12000 => OpusSampleRate::Hz12000,
            16000 => OpusSampleRate::Hz16000,
            24000 => OpusSampleRate::Hz24000,
            _ => OpusSampleRate::Hz48000,
        };
        let encoder_sample_rate = opus_rate as u32;
        let resampler = (sample_rate != encoder_sample_rate)
            .then(|| LinearResampler::new(sample_rate, encoder_sample_rate));

        let encoder =
            OpusEncoder::new(opus_rate, Channels::Mono, Application::Audio).map_err(|e| {
                AudioCaptureError::StartFailed(format!("Failed to init Opus encoder: {}", e))
            })?;
        let lookahead = encoder.lookahead().unwrap_or(0);
        let pre_skip = ((lookahead as u64) * 48_000 / u64::from(encoder_sample_rate))
            .min(u64::from(u16::MAX)) as u16;

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
            resampler,
            pcm_buffer: Vec::with_capacity(960),
            frame_samples: encoder_sample_rate as usize / 50, // 20ms frames
            encoder_sample_rate,
            granule_pos: 0,
            stream_serial,
            first_packet_logged: false,
            start_instant: Instant::now(),
            packet_count: 0,
            pending_packet: None,
        })
    }

    fn write(&mut self, samples: &[i16]) -> Result<(), AudioCaptureError> {
        if let Some(resampler) = &mut self.resampler {
            let input: Vec<_> = samples.iter().copied().map(f32::from).collect();
            let mut output = Vec::new();
            resampler.process(&input, &mut output);
            self.pcm_buffer
                .extend(output.into_iter().map(|sample| sample.round() as i16));
        } else {
            self.pcm_buffer.extend_from_slice(samples);
        }
        self.encode_buffered_frames()
    }

    fn encode_buffered_frames(&mut self) -> Result<(), AudioCaptureError> {
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

        // Ogg Opus granule positions always count 48 kHz samples.
        let frame_samples_48k = (frame.len() as u64) * 48_000 / u64::from(self.encoder_sample_rate);
        let next_granule = self.granule_pos + frame_samples_48k;

        let end_info = if self.packet_count % 20 == 19 {
            PacketWriteEndInfo::EndPage
        } else {
            PacketWriteEndInfo::NormalPacket
        };

        if let Some((packet, granule, ending)) = self.pending_packet.take() {
            self.writer
                .write_packet(packet, self.stream_serial, ending, granule)
                .map_err(|e| {
                    AudioCaptureError::StartFailed(format!("Failed to write Opus packet: {}", e))
                })?;
        }
        self.pending_packet = Some((output, next_granule, end_info));

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
        if let Some(resampler) = &mut self.resampler {
            let mut output = Vec::new();
            resampler.finish(&mut output);
            self.pcm_buffer
                .extend(output.into_iter().map(|sample| sample.round() as i16));
            self.encode_buffered_frames()?;
        }
        if !self.pcm_buffer.is_empty() {
            let mut frame = std::mem::take(&mut self.pcm_buffer);
            frame.resize(self.frame_samples, 0);
            self.encode_and_write(&frame)?;
        }

        let (last_packet, granule, _) = self.pending_packet.take().ok_or_else(|| {
            AudioCaptureError::StopFailed("No audio samples captured".to_string())
        })?;
        self.writer
            .write_packet(
                last_packet,
                self.stream_serial,
                PacketWriteEndInfo::EndStream,
                granule,
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

fn remove_incomplete_recording(path: &Path) {
    if let Err(error) = std::fs::remove_file(path) {
        log::warn!(
            "Failed to remove incomplete recording {}: {}",
            path.display(),
            error
        );
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
        // Checked only once opening failed, so the hotkey path never pays for
        // a device enumeration: with no microphone at all (a Mac mini, say)
        // the default device is a placeholder that fails to configure.
        self.start_inner(recordings_dir, capture_refinement_pcm)
            .map_err(|err| match err {
                AudioCaptureError::StartFailed(reason)
                    if !AudioInputDeviceManager::has_input_devices() =>
                {
                    log::warn!("No input device present; capture failed with: {}", reason);
                    AudioCaptureError::NoDevice
                }
                other => other,
            })
    }

    fn start_inner(
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
        #[cfg(target_os = "windows")]
        let config = {
            let session_protocol = windows_session_protocol();
            log::info!("Windows audio session protocol: {:?}", session_protocol);
            StreamConfig {
                buffer_size: windows_input_buffer_size(config.sample_rate.0, session_protocol),
                ..config
            }
        };
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
        let (viz_tx, viz_rx) = mpsc::channel(64);
        let (raw_tx, raw_rx) = std_mpsc::channel();
        let stop_tx = raw_tx.clone();
        let file_path = file_sink.as_ref().map(|sink| sink.path.clone());
        let debug_wav_path = wav_sink.as_ref().map(|wav| wav.path.clone());
        let callback_state = Arc::new(CallbackState::new(config.sample_rate.0));
        let producer = CallbackProducer {
            raw_tx,
            state: Arc::clone(&callback_state),
        };

        let stream = match Self::build_stream(device, config.clone(), sample_format, producer) {
            Ok(stream) => stream,
            Err(err) => {
                cleanup_unused_sinks(file_sink, wav_sink);
                return Err(err);
            }
        };
        if let Err(e) = stream.0.play() {
            callback_state.running.store(false, Ordering::SeqCst);
            let _ = stream.0.pause();
            drop(stream);
            cleanup_unused_sinks(file_sink, wav_sink);
            return Err(AudioCaptureError::StartFailed(format!(
                "Failed to start stream: {}",
                e
            )));
        }

        let worker_state = AudioCaptureWorker::new(
            tx,
            viz_tx,
            chunker,
            file_sink,
            wav_sink,
            capture_refinement_pcm,
            config.sample_rate.0,
        );
        let worker_callback_state = Arc::clone(&callback_state);
        let worker = match std::thread::Builder::new()
            .name("voicex-audio-worker".into())
            .spawn(move || worker_state.run(raw_rx, worker_callback_state))
        {
            Ok(handle) => handle,
            Err(err) => {
                callback_state.running.store(false, Ordering::SeqCst);
                let _ = stream.0.pause();
                drop(stream);
                if let Some(path) = file_path.as_deref() {
                    remove_incomplete_recording(path);
                }
                if let Some(path) = debug_wav_path.as_deref() {
                    remove_incomplete_recording(path);
                }
                return Err(AudioCaptureError::StartFailed(format!(
                    "Failed to spawn audio worker thread: {}",
                    err
                )));
            }
        };

        self.active = Some(ActiveCapture {
            stream: Some(stream),
            callback_state,
            stop_tx,
            worker: Some(worker),
            sample_rate: config.sample_rate.0,
            channels: output_channels,
        });

        log::info!(
            "Audio input opened ({} Hz, {} ch in -> {} ch out, format={:?}, buffer={:?}, device={:?})",
            config.sample_rate.0,
            input_channels,
            output_channels,
            sample_format,
            config.buffer_size,
            device_name
        );

        Ok(AudioCaptureHandle {
            receiver: rx,
            viz_receiver: viz_rx,
            file_path,
            sample_rate: config.sample_rate.0,
            channels: output_channels,
        })
    }

    /// Stop audio capture and finalize the recording.
    pub fn stop(&mut self) -> Result<AudioRecordingResult, AudioCaptureError> {
        let mut active = self.active.take().ok_or(AudioCaptureError::NotRunning)?;
        let elapsed = active.callback_state.start_instant.elapsed();
        let sample_rate = active.sample_rate;
        let channels = active.channels;

        // Pause the stream first so the audio backend stops invoking callbacks
        // and releases the device cleanly on macOS.
        if let Some(stream) = active.stream.take() {
            if let Err(e) = stream.0.pause() {
                log::warn!("Failed to pause audio stream: {}", e);
            }
            active.callback_state.running.store(false, Ordering::SeqCst);
            drop(stream);
        } else {
            active.callback_state.running.store(false, Ordering::SeqCst);
        }

        // Explicitly signal the worker to stop after draining every frame
        // already queued ahead of this sentinel. Do NOT rely on `drop(stream)`
        // releasing the callback closure (cpal 0.15.3 leaks `StreamInner` on
        // macOS non-default devices via an Arc cycle in its disconnect listener).
        let _ = active.stop_tx.send(WorkerCommand::Stop);

        let WorkerOutput {
            sink,
            wav_sink,
            refinement_pcm,
            samples_written: samples,
        } = active
            .worker
            .take()
            .ok_or_else(|| AudioCaptureError::StopFailed("Audio worker already stopped".into()))?
            .join()
            .map_err(|_| AudioCaptureError::StopFailed("Audio worker thread panicked".into()))?;

        let dropped = active.callback_state.dropped_callbacks();
        if dropped > 0 {
            log::warn!(
                "Audio capture dropped {} callback frame(s) because the worker queue was full",
                dropped
            );
        }

        let duration_ms = if sample_rate > 0 {
            samples.saturating_mul(1000) / (sample_rate as u64)
        } else {
            0
        };

        let mut bytes_written = 0;
        let mut path = None;
        let mut refinement_path = None;

        if let Some(sink) = sink {
            let sink_path = sink.path.clone();
            match sink.finish() {
                Ok((p, bytes)) => {
                    path = Some(p);
                    bytes_written = bytes;
                }
                Err(err) => {
                    log::error!(
                        "Failed to finalize audio file {}: {}",
                        sink_path.display(),
                        err
                    );
                    remove_incomplete_recording(&sink_path);
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

        log::info!(
            "Audio input timing (elapsed {} ms, captured {} ms, frames {})",
            elapsed.as_millis(),
            duration_ms,
            samples
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
        producer: CallbackProducer,
    ) -> Result<SendableStream, AudioCaptureError> {
        match sample_format {
            SampleFormat::I8 => {
                Self::build_typed_stream::<i8, _>(device, config, producer, |s| {
                    *s as f32 / i8::MAX as f32
                })
            }
            SampleFormat::U8 => {
                Self::build_typed_stream::<u8, _>(device, config, producer, |s| {
                    // Convert 0..255 to -1.0..1.0
                    (*s as f32 / u8::MAX as f32) * 2.0 - 1.0
                })
            }
            SampleFormat::I16 => {
                Self::build_typed_stream::<i16, _>(device, config, producer, |s| {
                    *s as f32 / i16::MAX as f32
                })
            }
            SampleFormat::U16 => {
                Self::build_typed_stream::<u16, _>(device, config, producer, |s| {
                    // Convert 0..65535 to -1.0..1.0
                    (*s as f32 / u16::MAX as f32) * 2.0 - 1.0
                })
            }
            SampleFormat::I32 => {
                Self::build_typed_stream::<i32, _>(device, config, producer, |s| {
                    *s as f32 / i32::MAX as f32
                })
            }
            SampleFormat::U32 => {
                Self::build_typed_stream::<u32, _>(device, config, producer, |s| {
                    (*s as f32 / u32::MAX as f32) * 2.0 - 1.0
                })
            }
            SampleFormat::I64 => {
                Self::build_typed_stream::<i64, _>(device, config, producer, |s| {
                    *s as f32 / i64::MAX as f32
                })
            }
            SampleFormat::U64 => {
                Self::build_typed_stream::<u64, _>(device, config, producer, |s| {
                    (*s as f32 / u64::MAX as f32) * 2.0 - 1.0
                })
            }
            SampleFormat::F32 => {
                Self::build_typed_stream::<f32, _>(device, config, producer, |s| *s)
            }
            SampleFormat::F64 => {
                Self::build_typed_stream::<f64, _>(device, config, producer, |s| *s as f32)
            }
            other => Err(AudioCaptureError::StartFailed(format!(
                "Unsupported sample format: {:?}",
                other
            ))),
        }
    }

    fn build_typed_stream<T, F>(
        device: cpal::Device,
        config: StreamConfig,
        producer: CallbackProducer,
        to_f32: F,
    ) -> Result<SendableStream, AudioCaptureError>
    where
        T: cpal::SizedSample,
        F: Fn(&T) -> f32 + Send + 'static,
    {
        let channels = config.channels as usize;
        let stream = device
            .build_input_stream(
                &config,
                move |data: &[T], _: &cpal::InputCallbackInfo| {
                    process_input(data, channels, &producer, &to_f32);
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

fn process_input<T, F>(data: &[T], channels: usize, producer: &CallbackProducer, to_f32: F)
where
    F: Fn(&T) -> f32,
{
    if !producer.state.is_running() {
        return;
    }
    producer.state.mark_first_frame_if_needed();

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

    producer.enqueue_mono(mono);
}

fn compute_visualization_bands(samples: &[i16], sample_rate: u32, level: f32) -> [f32; 8] {
    let mut bands = [0.0; 8];
    if samples.len() < 32 || sample_rate == 0 {
        return bands;
    }

    let max_samples = 256usize;
    let step = (samples.len() / max_samples).max(1);
    let mut frame = Vec::with_capacity(samples.len().div_ceil(step).min(max_samples));
    for sample in samples.iter().step_by(step).take(max_samples) {
        frame.push(*sample as f32 / i16::MAX as f32);
    }

    let n = frame.len();
    if n < 32 {
        return bands;
    }

    let denom = (n.saturating_sub(1)).max(1) as f32;
    for (index, value) in frame.iter_mut().enumerate() {
        let window = 0.5 - 0.5 * (TAU * index as f32 / denom).cos();
        *value *= window;
    }

    let bin_hz = sample_rate as f32 / n as f32;
    let nyquist = sample_rate as f32 * 0.5;
    let mut band_magnitudes = [0.0f32; 8];
    let mut max_magnitude = 1e-6f32;

    for (band_index, magnitude) in band_magnitudes.iter_mut().enumerate() {
        let low_hz = AUDIO_VIZ_BAND_EDGES[band_index].min(nyquist * 0.98);
        let high_hz = AUDIO_VIZ_BAND_EDGES[band_index + 1].min(nyquist * 0.98);
        if high_hz <= low_hz {
            continue;
        }

        let start_bin = ((low_hz / bin_hz).floor() as usize).clamp(1, n / 2);
        let end_bin = ((high_hz / bin_hz).ceil() as usize).clamp(start_bin, n / 2);
        let span = end_bin.saturating_sub(start_bin) + 1;
        let sample_bins = span.min(4);
        let bin_step = (span / sample_bins).max(1);
        let mut power_sum = 0.0f32;
        let mut count = 0usize;
        let mut bin = start_bin;

        while bin <= end_bin {
            let (re, im) = dft_bin(&frame, bin);
            power_sum += re * re + im * im;
            count += 1;
            if bin == end_bin {
                break;
            }
            bin = (bin + bin_step).min(end_bin);
        }

        if count == 0 {
            continue;
        }

        *magnitude = (power_sum / count as f32).sqrt();
        max_magnitude = max_magnitude.max(*magnitude);
    }

    let loudness = level.clamp(0.0, 1.0).powf(0.65);
    for (index, output) in bands.iter_mut().enumerate() {
        let normalized = (band_magnitudes[index] / max_magnitude).clamp(0.0, 1.0);
        let shaped = normalized.powf(0.75);
        *output = (shaped * (0.18 + loudness * 0.82)).clamp(0.0, 1.0);
    }

    bands
}

fn dft_bin(samples: &[f32], bin: usize) -> (f32, f32) {
    let n = samples.len() as f32;
    let mut re = 0.0f32;
    let mut im = 0.0f32;

    for (index, sample) in samples.iter().enumerate() {
        let phase = TAU * bin as f32 * index as f32 / n;
        re += *sample * phase.cos();
        im -= *sample * phase.sin();
    }

    (re / n, im / n)
}

impl Default for AudioCaptureService {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(target_os = "windows")]
fn windows_session_protocol() -> Option<u16> {
    use windows_sys::Win32::System::RemoteDesktop::{
        WTSClientProtocolType, WTSFreeMemory, WTSQuerySessionInformationW, WTS_CURRENT_SERVER_HANDLE,
        WTS_CURRENT_SESSION,
    };

    let mut buffer = std::ptr::null_mut();
    let mut bytes = 0;
    // WTS owns the returned USHORT buffer until WTSFreeMemory is called.
    let success = unsafe {
        WTSQuerySessionInformationW(
            WTS_CURRENT_SERVER_HANDLE,
            WTS_CURRENT_SESSION,
            WTSClientProtocolType,
            &mut buffer,
            &mut bytes,
        )
    };
    if success == 0 {
        log::warn!(
            "Failed to query Windows audio session protocol: {}",
            std::io::Error::last_os_error()
        );
        return None;
    }
    if buffer.is_null() || bytes != std::mem::size_of::<u16>() as u32 {
        if !buffer.is_null() {
            unsafe { WTSFreeMemory(buffer.cast()) };
        }
        log::warn!("Invalid Windows audio session protocol response ({} bytes)", bytes);
        return None;
    }

    let protocol = unsafe { buffer.read() };
    unsafe { WTSFreeMemory(buffer.cast()) };
    Some(protocol)
}

#[cfg(any(target_os = "windows", test))]
fn windows_input_buffer_size(sample_rate: u32, session_protocol: Option<u16>) -> cpal::BufferSize {
    // WTSClientProtocolType returns 2 for RDP. The larger WASAPI buffer
    // provides capacity for redirected audio arriving in bursts.
    const RDP_PROTOCOL: u16 = 2;
    if session_protocol == Some(RDP_PROTOCOL) {
        cpal::BufferSize::Fixed(sample_rate / 2)
    } else {
        cpal::BufferSize::Default
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

/// Write raw PCM16 to a temporary WAV file.
///
/// Shared with ASR backends whose CLI only accepts WAV, since VoiceX records
/// OGG/Opus by default.
pub fn write_temp_wav(
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

    #[error("Audio capture cancelled before start completed")]
    Cancelled,

    #[error("Failed to start capture: {0}")]
    StartFailed(String),

    #[error("Failed to stop capture: {0}")]
    StopFailed(String),
}

#[cfg(test)]
#[test]
fn only_rdp_uses_the_larger_windows_input_buffer() {
    assert_eq!(
        windows_input_buffer_size(44_100, Some(2)),
        cpal::BufferSize::Fixed(22_050)
    );
    assert_eq!(
        windows_input_buffer_size(48_000, Some(2)),
        cpal::BufferSize::Fixed(24_000)
    );
    for session_protocol in [Some(0), Some(1), None] {
        assert_eq!(
            windows_input_buffer_size(44_100, session_protocol),
            cpal::BufferSize::Default
        );
    }
}

#[cfg(test)]
mod opus_finalization_tests {
    use super::*;
    use ogg::reading::PacketReader;

    #[tokio::test]
    #[ignore = "requires a DeepSeek API key and network"]
    async fn live_deepseek_accepts_new_recording() {
        let key = std::env::var("DEEPSEEK_API_KEY").unwrap();
        let dir = std::env::temp_dir().join(format!("voicex-ogg-live-{}", uuid::Uuid::new_v4()));
        let mut sink = OggOpusSink::create(&dir, 16_000).unwrap();
        sink.write(&vec![0; 16_000]).unwrap();
        let (path, _) = sink.finish().unwrap();
        let result = crate::asr::DeepSeekTranscriptionClient::new(key).transcribe_file(&path).await;
        std::fs::remove_dir_all(dir).unwrap();
        assert!(result.is_ok(), "DeepSeek rejected VoiceX recording: {:?}", result.err());
    }

    #[test]
    fn recording_ends_on_the_last_audio_packet() {
        // Exercise a partial frame and page boundaries, including an exact
        // twenty-packet page where EOS used to occupy its own page.
        for samples in [1, 320, 320 * 19, 320 * 20, 320 * 21 + 17] {
            let dir = std::env::temp_dir().join(format!("voicex-ogg-test-{}", uuid::Uuid::new_v4()));
            let mut sink = OggOpusSink::create(&dir, 16_000).unwrap();
            sink.write(&vec![0; samples]).unwrap();
            let (path, _) = sink.finish().unwrap();
            let mut reader = PacketReader::new(File::open(&path).unwrap());
            let mut packets = Vec::new();
            while let Some(packet) = reader.read_packet().unwrap() { packets.push(packet); }
            assert_eq!(packets.len(), 2 + samples.div_ceil(320));
            assert!(packets.last().unwrap().last_in_stream());
            assert!(packets.iter().all(|packet| !packet.data.is_empty()));
            let decoded = crate::asr::ogg_decoder::decode_ogg_opus_to_pcm16k(&path).unwrap();
            assert!(!decoded.is_empty());
            std::fs::remove_dir_all(dir).unwrap();
        }
    }

    #[test]
    fn recording_resamples_unsupported_device_rates_across_callbacks() {
        for rate in [44_100, 48_000, 96_000] {
            for sample_count in [1, rate / 50, rate] {
                let dir =
                    std::env::temp_dir().join(format!("voicex-ogg-test-{}", uuid::Uuid::new_v4()));
                let mut sink = OggOpusSink::create(&dir, rate).unwrap();
                let input: Vec<_> = (0..sample_count)
                    .map(|index| {
                        (12_000.0 * (TAU * 440.0 * index as f32 / rate as f32).cos()) as i16
                    })
                    .collect();
                for chunk in input.chunks(127) {
                    sink.write(chunk).unwrap();
                }

                let (path, _) = sink.finish().unwrap();
                let mut reader = PacketReader::new(File::open(&path).unwrap());
                let head = reader.read_packet().unwrap().unwrap();
                assert_eq!(&head.data[12..16], &rate.to_le_bytes());
                assert_eq!(u16::from_le_bytes([head.data[10], head.data[11]]), 312);
                reader.read_packet().unwrap().unwrap(); // OpusTags
                let expected_packets = sample_count.div_ceil(rate / 50);
                let mut packet_count = 0;
                while let Some(packet) = reader.read_packet().unwrap() {
                    packet_count += 1;
                    assert!(!packet.data.is_empty());
                    assert_eq!(packet.last_in_stream(), packet_count == expected_packets);
                    if packet.last_in_stream() {
                        assert_eq!(packet.absgp_page(), u64::from(expected_packets) * 960);
                    }
                }
                assert_eq!(packet_count, expected_packets);
                drop(reader);
                let pcm = crate::asr::ogg_decoder::decode_ogg_opus_to_pcm16k(&path).unwrap();
                assert!(!pcm.is_empty());
                if sample_count == rate {
                    assert!(pcm.iter().any(|&byte| byte != 0));
                }
                std::fs::remove_dir_all(dir).unwrap();
            }
        }
    }

    #[test]
    fn audio_worker_drains_all_frames_and_stops_even_if_callback_producer_is_retained() {
        let dir =
            std::env::temp_dir().join(format!("voicex-worker-test-{}", uuid::Uuid::new_v4()));
        let sample_rate = 16_000u32;
        let chunk_bytes = bytes_per_ms(sample_rate, 100); // 3200 bytes = 1600 samples
        let (tx, mut rx) = mpsc::channel(32);
        let (viz_tx, mut viz_rx) = mpsc::channel(32);
        let (raw_tx, raw_rx) = std_mpsc::channel();
        let stop_tx = raw_tx.clone();

        let callback_state = Arc::new(CallbackState::new(sample_rate));
        let sink = OggOpusSink::create(&dir, sample_rate).unwrap();
        let worker = AudioCaptureWorker::new(
            tx,
            viz_tx,
            AudioChunker::new(chunk_bytes),
            Some(sink),
            None,
            true,
            sample_rate,
        );
        let worker_state = Arc::clone(&callback_state);
        let handle = std::thread::spawn(move || worker.run(raw_rx, worker_state));

        let producer = CallbackProducer {
            raw_tx,
            state: Arc::clone(&callback_state),
        };

        // Push 2,000 stereo frames across uneven callbacks (100ms chunk = 1,600 samples,
        // leaving a 400-sample tail that must be flushed before `rx` closes).
        let frame_counts = [512usize, 700, 788];
        let mut sample_idx = 0usize;
        for count in frame_counts {
            let mut stereo = Vec::with_capacity(count * 2);
            for _ in 0..count {
                let value = 0.4 * (TAU * 440.0 * sample_idx as f32 / sample_rate as f32).sin();
                stereo.push(value);
                stereo.push(value);
                sample_idx += 1;
            }
            process_input(&stereo, 2, &producer, |s| *s);
        }

        // Deliberately KEEP `producer` alive while sending `Stop` to mirror
        // cpal 0.15.3 retaining `StreamInner` on macOS non-default devices.
        callback_state.running.store(false, Ordering::SeqCst);
        stop_tx.send(WorkerCommand::Stop).unwrap();

        let output = handle.join().unwrap();
        drop(producer);
        assert_eq!(output.samples_written, 2_000);
        assert_eq!(callback_state.dropped_callbacks(), 0);

        // Drain emitted ASR chunks: one full 3200-byte chunk + one flushed 800-byte tail.
        let mut chunks = Vec::new();
        while let Some(chunk) = rx.blocking_recv() {
            chunks.push(chunk);
        }
        assert_eq!(chunks.len(), 2);
        assert_eq!(chunks[0].len(), 3_200);
        assert_eq!(chunks[1].len(), 800);

        // Visualization channel received one frame per callback and then closed cleanly.
        let mut viz_frames = Vec::new();
        while let Some(frame) = viz_rx.blocking_recv() {
            viz_frames.push(frame);
        }
        assert_eq!(viz_frames.len(), frame_counts.len());
        assert!(viz_frames.iter().all(|f| f.level > 0.0 && f.bands.len() == 8));

        // Refinement PCM matches total bytes (2000 samples * 2 bytes).
        let refinement_pcm = output.refinement_pcm.expect("refinement PCM captured");
        assert_eq!(refinement_pcm.len(), 4_000);

        // Finalized Opus recording is valid and decodable.
        let (path, bytes) = output.sink.expect("opus sink present").finish().unwrap();
        assert!(bytes > 0);
        let decoded = crate::asr::ogg_decoder::decode_ogg_opus_to_pcm16k(&path).unwrap();
        assert!(!decoded.is_empty());
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn callback_producer_drops_nonblockingly_when_queue_is_full_and_respects_stop() {
        let (raw_tx, raw_rx) = std_mpsc::channel();
        let callback_state = Arc::new(CallbackState::with_capacity(16_000, 2));
        let producer = CallbackProducer {
            raw_tx,
            state: Arc::clone(&callback_state),
        };

        let samples = [0.25f32; 64];
        for _ in 0..5 {
            process_input(&samples, 1, &producer, |s| *s);
        }
        assert_eq!(callback_state.dropped_callbacks(), 3);

        // Once stopped, callbacks must immediately return without enqueueing or dropping.
        callback_state.running.store(false, Ordering::SeqCst);
        process_input(&samples, 1, &producer, |s| *s);
        assert_eq!(callback_state.dropped_callbacks(), 3);

        drop(producer);
        let mut received = 0usize;
        while let Ok(WorkerCommand::Frame(_)) = raw_rx.recv() {
            received += 1;
        }
        assert_eq!(received, 2);
    }
}
