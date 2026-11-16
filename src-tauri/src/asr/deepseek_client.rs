//! DeepSeek's file-based speech recognition API.
use super::AsrError;
use reqwest::multipart::{Form, Part};
use serde::Deserialize;
use std::path::Path;

const ENDPOINT: &str = "https://api.deepseek.com/v1/tools/asr";
const MAX_BYTES: usize = 8 * 1024 * 1024;

pub struct DeepSeekTranscriptionClient {
    api_key: String,
    http: reqwest::Client,
}

#[derive(Deserialize)]
struct Response {
    text: String,
}

impl DeepSeekTranscriptionClient {
    pub fn new(api_key: String) -> Self {
        Self {
            api_key,
            http: reqwest::Client::new(),
        }
    }

    pub async fn transcribe_file(&self, path: &Path) -> Result<String, AsrError> {
        if self.api_key.trim().is_empty() {
            return Err(AsrError::ConnectionFailed(
                "DeepSeek API Key is missing".into(),
            ));
        }
        let mut bytes = tokio::fs::read(path)
            .await
            .map_err(|e| AsrError::ConnectionFailed(format!("Failed to read audio file: {e}")))?;
        if bytes.len() > MAX_BYTES {
            return Err(AsrError::ProtocolError(
                "DeepSeek audio exceeds maximum 8 MiB".into(),
            ));
        }
        let (name, mime) = match path
            .extension()
            .and_then(|v| v.to_str())
            .unwrap_or("")
            .to_ascii_lowercase()
            .as_str()
        {
            "opus" | "ogg" => ("audio.ogg", "audio/ogg"),
            "wav" => ("audio.wav", "audio/wav"),
            _ => {
                return Err(AsrError::ProtocolError(
                    "DeepSeek requires WAV or Ogg Opus audio".into(),
                ))
            }
        };
        if mime == "audio/ogg" {
            bytes = normalize_legacy_opus(bytes).map_err(AsrError::ProtocolError)?;
        }
        let part = Part::bytes(bytes)
            .file_name(name)
            .mime_str(mime)
            .map_err(|e| AsrError::ProtocolError(e.to_string()))?;
        let response = self
            .http
            .post(ENDPOINT)
            .timeout(std::time::Duration::from_secs(90))
            .bearer_auth(&self.api_key)
            .multipart(Form::new().part("file", part))
            .send()
            .await
            .map_err(|e| AsrError::ConnectionFailed(e.to_string()))?;
        let status = response.status();
        let body = response
            .bytes()
            .await
            .map_err(|e| AsrError::ConnectionFailed(e.to_string()))?;
        if !status.is_success() {
            return Err(AsrError::ServerError(format!(
                "DeepSeek HTTP {}: {}",
                status.as_u16(),
                String::from_utf8_lossy(&body)
            )));
        }
        let result: Response = serde_json::from_slice(&body)
            .map_err(|e| AsrError::ProtocolError(format!("Invalid DeepSeek response: {e}")))?;
        Ok(result.text.trim().to_string())
    }
}

/// Old VoiceX recordings wrote an empty Opus packet as the end marker. Move
/// EOS onto the preceding audio packet, preserving audio bytes and page
/// granule positions. Valid files pass through byte-for-byte unchanged.
fn normalize_legacy_opus(bytes: Vec<u8>) -> Result<Vec<u8>, String> {
    use ogg::reading::PacketReader;
    use ogg::writing::{PacketWriteEndInfo, PacketWriter};
    use std::io::Cursor;

    let mut reader = PacketReader::new(Cursor::new(&bytes));
    let mut packets = Vec::new();
    while let Some(packet) = reader
        .read_packet()
        .map_err(|e| format!("Invalid Ogg audio: {e}"))?
    {
        packets.push(packet);
    }
    let needs_repair = packets
        .last()
        .is_some_and(|p| p.data.is_empty() && p.last_in_stream());
    if !needs_repair {
        return Ok(bytes);
    }
    if packets.len() < 4
        || !packets[0].data.starts_with(b"OpusHead")
        || !packets[1].data.starts_with(b"OpusTags")
    {
        return Err("Invalid Ogg Opus stream: no audio before end marker".into());
    }
    let end = packets.pop().expect("empty end marker exists");
    let serial = end.stream_serial();
    if packets
        .iter()
        .any(|p| p.stream_serial() != serial || p.data.is_empty() || p.last_in_stream())
    {
        return Err(
            "Cannot normalize Ogg Opus with multiple streams or empty audio packets".into(),
        );
    }
    let last_index = packets.len() - 1;
    let mut writer = PacketWriter::new(Vec::new());
    for (index, packet) in packets.into_iter().enumerate() {
        let (ending, granule) = if index == last_index {
            (PacketWriteEndInfo::EndStream, end.absgp_page())
        } else if packet.last_in_page() {
            (PacketWriteEndInfo::EndPage, packet.absgp_page())
        } else {
            (PacketWriteEndInfo::NormalPacket, packet.absgp_page())
        };
        writer
            .write_packet(packet.data, serial, ending, granule)
            .map_err(|e| format!("Failed to normalize Ogg Opus: {e}"))?;
    }
    log::info!("Normalized legacy VoiceX Ogg Opus end marker for DeepSeek");
    Ok(writer.into_inner())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn read_packets(bytes: &[u8]) -> Vec<ogg::Packet> {
        let mut reader = ogg::reading::PacketReader::new(std::io::Cursor::new(bytes));
        let mut packets = Vec::new();
        while let Some(packet) = reader.read_packet().unwrap() {
            packets.push(packet);
        }
        packets
    }

    #[test]
    fn removes_only_the_legacy_empty_end_packet() {
        use ogg::writing::{PacketWriteEndInfo, PacketWriter};
        let source = include_bytes!("../../provider-probe.ogg").to_vec();
        assert_eq!(normalize_legacy_opus(source.clone()).unwrap(), source);
        let packets = read_packets(&source);
        let serial = packets[0].stream_serial();
        let granule = packets.last().unwrap().absgp_page();
        let mut writer = PacketWriter::new(Vec::new());
        for packet in packets {
            let ending = if packet.last_in_page() {
                PacketWriteEndInfo::EndPage
            } else {
                PacketWriteEndInfo::NormalPacket
            };
            let position = packet.absgp_page();
            writer
                .write_packet(packet.data, serial, ending, position)
                .unwrap();
        }
        writer
            .write_packet(Vec::new(), serial, PacketWriteEndInfo::EndStream, granule)
            .unwrap();
        let legacy = writer.into_inner();
        assert!(read_packets(&legacy).last().unwrap().data.is_empty());
        let repaired = normalize_legacy_opus(legacy).unwrap();
        let original_packets = read_packets(&source);
        let repaired_packets = read_packets(&repaired);
        assert_eq!(original_packets.len(), repaired_packets.len());
        assert!(repaired_packets.last().unwrap().last_in_stream());
        assert_eq!(repaired_packets.last().unwrap().absgp_page(), granule);
        for (a, b) in original_packets.iter().zip(&repaired_packets) {
            assert_eq!(a.data, b.data);
        }
    }

    #[tokio::test]
    #[ignore = "requires an existing recording, DeepSeek API key, and network"]
    async fn live_recognizes_recording_from_path() {
        let key = std::env::var("DEEPSEEK_API_KEY").unwrap();
        let path = std::env::var_os("VOICEX_DEEPSEEK_TEST_AUDIO").unwrap();
        let text = DeepSeekTranscriptionClient::new(key)
            .transcribe_file(Path::new(&path))
            .await
            .unwrap();
        assert!(!text.is_empty());
    }

    #[tokio::test]
    #[ignore = "requires a DeepSeek API key and network"]
    async fn live_transcribes_voicex_probe() {
        let key = std::env::var("DEEPSEEK_API_KEY").unwrap();
        let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("provider-probe.ogg");
        let mut config = crate::asr::AsrConfig::default();
        config.provider_type = crate::asr::AsrProviderType::DeepSeek;
        config.deepseek_api_key = key;
        let outcome = crate::asr::transcribe_audio_path_detailed(
            &path,
            &mut config,
            tokio_util::sync::CancellationToken::new(),
        )
        .await
        .unwrap();
        assert!(!outcome.text.is_empty());
        assert_eq!(outcome.model_name.as_deref(), Some("DeepSeek ASR"));
    }
}
