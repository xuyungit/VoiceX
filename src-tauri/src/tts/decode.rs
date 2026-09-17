//! Streaming MP3 decode for cloud speech backends.
//!
//! The provider hands back base64 audio in many small chunks (plan §5.4 measured
//! 112 of them for a 45-second read), and those chunk boundaries fall wherever
//! the transport put them — not on MP3 frame boundaries. Reassembling the byte
//! stream is this module's job; finding frames inside it is symphonia's.
//!
//! Decoding blocks, so it runs on its own thread. It ends when the producer
//! drops its sender, which is also how cancellation reaches a decoder parked in
//! `recv`.

use std::io::{self, Read, Seek, SeekFrom};
use std::sync::mpsc::{Receiver, RecvTimeoutError};
use std::sync::Mutex;
use std::time::Duration;

use symphonia::core::audio::SampleBuffer;
use symphonia::core::codecs::DecoderOptions;
use symphonia::core::errors::Error as SymphoniaError;
use symphonia::core::formats::FormatOptions;
use symphonia::core::io::{MediaSource, MediaSourceStream};
use symphonia::core::meta::MetadataOptions;
use symphonia::core::probe::Hint;

use super::CancelToken;

/// How long to park in `recv` before re-checking cancellation. Short enough
/// that stopping feels immediate, long enough not to spin.
pub const RECV_POLL: Duration = Duration::from_millis(120);

#[derive(Debug, Clone, thiserror::Error)]
pub enum DecodeError {
    #[error("The audio stream carries no decodable track")]
    NoTrack,

    #[error("Unsupported audio codec: {0}")]
    UnsupportedCodec(String),

    #[error("Audio decode failed: {0}")]
    Decode(String),

    #[error("Provider returned {actual} Hz audio but {requested} Hz was requested")]
    SampleRateMismatch { requested: u32, actual: u32 },
}

impl DecodeError {
    pub fn code(&self) -> &'static str {
        match self {
            DecodeError::NoTrack => "no_audio_track",
            DecodeError::UnsupportedCodec(_) => "unsupported_codec",
            DecodeError::Decode(_) => "decode_failed",
            DecodeError::SampleRateMismatch { .. } => "sample_rate_mismatch",
        }
    }
}

/// A byte stream assembled from chunks arriving on a channel.
///
/// Presents whatever arrives as one contiguous stream, so a decoder never sees
/// the transport's chunk boundaries. Not seekable: the bytes are consumed as
/// they arrive and are never held for a second pass.
pub struct ChunkSource {
    /// `MediaSource` demands `Sync` and a plain `Receiver` is not, so it lives
    /// behind a mutex. Nothing ever contends for it: reads go through
    /// `&mut self` and take the receiver with `get_mut`, which never locks.
    rx: Mutex<Receiver<Vec<u8>>>,
    current: Vec<u8>,
    offset: usize,
    token: CancelToken,
    finished: bool,
}

impl ChunkSource {
    pub fn new(rx: Receiver<Vec<u8>>, token: CancelToken) -> Self {
        Self {
            rx: Mutex::new(rx),
            current: Vec::new(),
            offset: 0,
            token,
            finished: false,
        }
    }
}

impl Read for ChunkSource {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        if buf.is_empty() {
            return Ok(0);
        }

        loop {
            // Serve from the chunk in hand first. A caller asking for more than
            // one chunk holds gets a short read, which `Read` allows and
            // symphonia's buffered reader handles.
            if self.offset < self.current.len() {
                let n = (self.current.len() - self.offset).min(buf.len());
                buf[..n].copy_from_slice(&self.current[self.offset..self.offset + n]);
                self.offset += n;
                return Ok(n);
            }

            if self.finished {
                return Ok(0);
            }

            let received = match self.rx.get_mut() {
                Ok(rx) => rx.recv_timeout(RECV_POLL),
                // Only reachable if a previous read panicked mid-recv.
                Err(_) => Err(RecvTimeoutError::Disconnected),
            };

            match received {
                Ok(chunk) => {
                    self.current = chunk;
                    self.offset = 0;
                }
                // The producer is gone: end of stream, not an error.
                Err(RecvTimeoutError::Disconnected) => {
                    self.finished = true;
                    return Ok(0);
                }
                Err(RecvTimeoutError::Timeout) => {
                    if self.token.is_cancelled() {
                        self.finished = true;
                        return Ok(0);
                    }
                }
            }
        }
    }
}

impl Seek for ChunkSource {
    fn seek(&mut self, _pos: SeekFrom) -> io::Result<u64> {
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "the speech audio stream is not seekable",
        ))
    }
}

impl MediaSource for ChunkSource {
    fn is_seekable(&self) -> bool {
        false
    }

    fn byte_len(&self) -> Option<u64> {
        None
    }
}

/// Decode an MP3 stream, handing mono samples to `on_samples` as they come.
///
/// `on_samples` receives the decoded sample rate alongside every batch and
/// returns false to abort. Returns the total number of mono samples decoded.
///
/// `requested_rate` is what we asked the provider for; a mismatch is reported
/// rather than played, because playing it would silently come out at the wrong
/// pitch and speed — much harder to diagnose than an error code.
pub fn decode_mp3_stream(
    source: ChunkSource,
    requested_rate: u32,
    mut on_samples: impl FnMut(&[f32]) -> bool,
) -> Result<u64, DecodeError> {
    let stream = MediaSourceStream::new(Box::new(source), Default::default());

    let mut hint = Hint::new();
    hint.with_extension("mp3");
    hint.mime_type("audio/mpeg");

    let probed = symphonia::default::get_probe()
        .format(
            &hint,
            stream,
            &FormatOptions::default(),
            &MetadataOptions::default(),
        )
        .map_err(|err| DecodeError::Decode(err.to_string()))?;

    let mut format = probed.format;
    let track = format.default_track().ok_or(DecodeError::NoTrack)?;
    let track_id = track.id;
    let mut decoder = symphonia::default::get_codecs()
        .make(&track.codec_params, &DecoderOptions::default())
        .map_err(|err| DecodeError::UnsupportedCodec(err.to_string()))?;

    let mut interleaved: Option<SampleBuffer<f32>> = None;
    let mut mono: Vec<f32> = Vec::new();
    let mut total = 0u64;

    loop {
        let packet = match format.next_packet() {
            Ok(packet) => packet,
            // The producer closed the channel: a clean end of stream.
            Err(SymphoniaError::IoError(err)) if err.kind() == io::ErrorKind::UnexpectedEof => {
                break
            }
            Err(SymphoniaError::ResetRequired) => break,
            Err(err) => return Err(DecodeError::Decode(err.to_string())),
        };

        if packet.track_id() != track_id {
            continue;
        }

        let audio = match decoder.decode(&packet) {
            Ok(audio) => audio,
            // A corrupt frame is recoverable; the next one usually decodes.
            Err(SymphoniaError::DecodeError(err)) => {
                log::debug!("Skipping an undecodable MP3 frame: {err}");
                continue;
            }
            Err(SymphoniaError::IoError(err)) if err.kind() == io::ErrorKind::UnexpectedEof => {
                break
            }
            Err(err) => return Err(DecodeError::Decode(err.to_string())),
        };

        let spec = *audio.spec();
        if spec.rate != requested_rate {
            return Err(DecodeError::SampleRateMismatch {
                requested: requested_rate,
                actual: spec.rate,
            });
        }

        let channels = spec.channels.count().max(1);
        let buffer =
            interleaved.get_or_insert_with(|| SampleBuffer::new(audio.capacity() as u64, spec));
        buffer.copy_interleaved_ref(audio);
        let samples = buffer.samples();

        // Providers send mono today, but a stereo voice would otherwise play at
        // double speed through the mono-oriented playback path.
        mono.clear();
        if channels == 1 {
            mono.extend_from_slice(samples);
        } else {
            mono.reserve(samples.len() / channels);
            for frame in samples.chunks_exact(channels) {
                mono.push(frame.iter().sum::<f32>() / channels as f32);
            }
        }

        total += mono.len() as u64;
        if !on_samples(&mono) {
            return Ok(total);
        }
    }

    Ok(total)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tts::SessionSlot;
    use std::sync::mpsc;

    fn source_of(chunks: Vec<Vec<u8>>) -> (ChunkSource, SessionSlot) {
        let slot = SessionSlot::default();
        let token = slot.claim();
        let (tx, rx) = mpsc::channel();
        for chunk in chunks {
            tx.send(chunk).unwrap();
        }
        drop(tx);
        (ChunkSource::new(rx, token), slot)
    }

    fn read_all(mut source: ChunkSource, read_size: usize) -> Vec<u8> {
        let mut out = Vec::new();
        let mut buf = vec![0u8; read_size];
        loop {
            match source.read(&mut buf).unwrap() {
                0 => return out,
                n => out.extend_from_slice(&buf[..n]),
            }
        }
    }

    #[test]
    fn chunk_boundaries_never_reach_the_decoder() {
        // The whole point of the module: however the transport split the bytes,
        // and whatever size the decoder reads in, the same stream comes out.
        // This is where the cost of choosing MP3 over PCM lives (plan §5.4).
        let whole: Vec<u8> = (0..=255u8).cycle().take(1000).collect();

        for split in [1usize, 7, 64, 333, 1000] {
            let chunks: Vec<Vec<u8>> = whole.chunks(split).map(|c| c.to_vec()).collect();
            for read_size in [1usize, 3, 128, 4096] {
                let (source, _slot) = source_of(chunks.clone());
                assert_eq!(
                    read_all(source, read_size),
                    whole,
                    "split={split} read_size={read_size}"
                );
            }
        }
    }

    #[test]
    fn a_closed_channel_reads_as_end_of_stream_not_an_error() {
        let (source, _slot) = source_of(vec![]);
        assert!(read_all(source, 16).is_empty());
    }

    #[test]
    fn empty_chunks_do_not_terminate_the_stream() {
        // A keepalive or an empty audio field must not be mistaken for EOF, or
        // the tail of the utterance would be silently dropped.
        let (source, _slot) = source_of(vec![b"ab".to_vec(), Vec::new(), b"cd".to_vec()]);
        assert_eq!(read_all(source, 8), b"abcd");
    }

    #[test]
    fn cancelling_releases_a_decoder_waiting_for_more_audio() {
        // Without this the decode thread would sit in recv until the network
        // side happened to finish, long after the user pressed stop.
        let slot = SessionSlot::default();
        let token = slot.claim();
        let (tx, rx) = mpsc::channel::<Vec<u8>>();
        let mut source = ChunkSource::new(rx, token);

        slot.release();

        let mut buf = [0u8; 8];
        assert_eq!(source.read(&mut buf).unwrap(), 0);
        drop(tx);
    }

    #[test]
    fn the_stream_is_not_seekable() {
        // symphonia asks; answering yes would make it try to rewind bytes we
        // have already consumed and thrown away.
        let (source, _slot) = source_of(vec![b"x".to_vec()]);
        assert!(!source.is_seekable());
        assert_eq!(source.byte_len(), None);
    }
}
