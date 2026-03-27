//! Google Cloud Speech-to-Text V2 gRPC streaming client.
//!
//! Authentication: uses a Service Account JSON key pasted into the app settings.
//! The app signs a JWT with RS256 and exchanges it for an OAuth2 access token.

use std::error::Error as StdError;
use std::sync::Arc;
use std::time::Duration;

use base64::engine::general_purpose::{STANDARD, URL_SAFE_NO_PAD};
use base64::Engine;
use ring::rand::SystemRandom;
use ring::signature::{RsaKeyPair, RSA_PKCS1_SHA256};
use tokio::sync::mpsc::Receiver;
use tokio::sync::Mutex;
use tokio_stream::wrappers::ReceiverStream;
use tonic::metadata::MetadataValue;
use tonic::transport::{Channel, ClientTlsConfig};

use super::audio_utils::resample_to_16k;
use super::config::AsrConfig;
use super::protocol::{AsrError, AsrEvent};

/// Cached access token with expiry.
struct CachedToken {
    token: String,
    /// SA JSON hash — invalidate cache when credentials change.
    sa_hash: u64,
    expires_at: std::time::Instant,
}

static TOKEN_CACHE: std::sync::LazyLock<Mutex<Option<CachedToken>>> =
    std::sync::LazyLock::new(|| Mutex::new(None));

/// Cached gRPC channel, keyed by endpoint URL.
struct CachedChannel {
    endpoint: String,
    channel: Channel,
}

static CHANNEL_CACHE: std::sync::LazyLock<Mutex<Option<CachedChannel>>> =
    std::sync::LazyLock::new(|| Mutex::new(None));

/// Generated proto types — mirror the proto package hierarchy so that
/// cross-package `super::` references resolve correctly.
#[doc(hidden)]
#[allow(clippy::all, unused_imports, dead_code)]
mod google {
    pub mod longrunning {
        include!(concat!(env!("OUT_DIR"), "/google.longrunning.rs"));
    }
    pub mod rpc {
        include!(concat!(env!("OUT_DIR"), "/google.rpc.rs"));
    }
    pub mod cloud {
        pub mod speech {
            pub mod v2 {
                include!(concat!(env!("OUT_DIR"), "/google.cloud.speech.v2.rs"));
            }
        }
    }
}

use google::cloud::speech::v2::speech_client::SpeechClient;
use google::cloud::speech::v2::streaming_recognize_request::StreamingRequest;
use google::cloud::speech::v2::{
    ExplicitDecodingConfig, PhraseSet, RecognitionConfig, SpeechAdaptation,
    StreamingRecognitionConfig, StreamingRecognitionFeatures, StreamingRecognizeRequest,
    StreamingRecognizeResponse,
};

/// Get a cached gRPC channel or create a new one (eagerly connecting).
async fn get_or_create_channel(endpoint_url: &str) -> Result<Channel, AsrError> {
    // Check cache
    {
        let cache = CHANNEL_CACHE.lock().await;
        if let Some(cached) = cache.as_ref() {
            if cached.endpoint == endpoint_url {
                log::debug!("Google STT: reusing cached gRPC channel");
                return Ok(cached.channel.clone());
            }
        }
    }

    // Cache miss — create and eagerly connect
    let tls_domain = endpoint_url
        .strip_prefix("https://")
        .unwrap_or(endpoint_url);
    let tls_config = ClientTlsConfig::new()
        .domain_name(tls_domain)
        .with_enabled_roots();

    let channel = Channel::from_shared(endpoint_url.to_string())
        .map_err(|e| AsrError::ConnectionFailed(format!("Invalid endpoint URL: {e}")))?
        .tls_config(tls_config)
        .map_err(|e| AsrError::ConnectionFailed(format!("TLS config error: {e}")))?
        .connect_timeout(Duration::from_secs(10))
        .timeout(Duration::from_secs(300))
        .connect()
        .await
        .map_err(|e| {
            AsrError::ConnectionFailed(format!("gRPC connect to {endpoint_url} failed: {e:?}"))
        })?;

    log::info!("Google STT: new gRPC channel connected to {}", endpoint_url);

    // Store in cache
    {
        let mut cache = CHANNEL_CACHE.lock().await;
        *cache = Some(CachedChannel {
            endpoint: endpoint_url.to_string(),
            channel: channel.clone(),
        });
    }

    Ok(channel)
}

/// Simple hash for cache invalidation when SA JSON changes.
fn hash_sa_json(sa_json: &str) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    sa_json.hash(&mut hasher);
    hasher.finish()
}

/// Obtain an OAuth2 access token from a Service Account JSON key.
/// Caches the token for up to 50 minutes (tokens are valid for 60 min).
async fn get_service_account_token(sa_json: &str) -> Result<String, AsrError> {
    let sa_hash = hash_sa_json(sa_json);

    // Check cache
    {
        let cache = TOKEN_CACHE.lock().await;
        if let Some(cached) = cache.as_ref() {
            if cached.sa_hash == sa_hash && cached.expires_at > std::time::Instant::now() {
                log::debug!("Google STT: using cached access token");
                return Ok(cached.token.clone());
            }
        }
    }

    // Cache miss — exchange JWT for a fresh token
    let token = exchange_sa_jwt(sa_json).await?;

    // Store in cache (expire 10 min early to avoid edge cases)
    {
        let mut cache = TOKEN_CACHE.lock().await;
        *cache = Some(CachedToken {
            token: token.clone(),
            sa_hash,
            expires_at: std::time::Instant::now() + Duration::from_secs(50 * 60),
        });
    }

    Ok(token)
}

/// Sign a JWT with the SA private key and exchange it for an access token.
async fn exchange_sa_jwt(sa_json: &str) -> Result<String, AsrError> {
    let creds: serde_json::Value = serde_json::from_str(sa_json)
        .map_err(|e| AsrError::ConnectionFailed(format!("Invalid Service Account JSON: {e}")))?;

    let client_email = creds["client_email"].as_str().ok_or_else(|| {
        AsrError::ConnectionFailed("Missing 'client_email' in Service Account JSON".to_string())
    })?;
    let private_key_pem = creds["private_key"].as_str().ok_or_else(|| {
        AsrError::ConnectionFailed("Missing 'private_key' in Service Account JSON".to_string())
    })?;
    let token_uri = creds["token_uri"]
        .as_str()
        .unwrap_or("https://oauth2.googleapis.com/token");

    // Parse PEM private key → DER PKCS#8
    let pem_body: String = private_key_pem
        .lines()
        .filter(|line| !line.starts_with("-----"))
        .collect::<Vec<_>>()
        .join("");
    let der = STANDARD.decode(&pem_body).map_err(|e| {
        AsrError::ConnectionFailed(format!("Failed to decode private key base64: {e}"))
    })?;

    let key_pair = RsaKeyPair::from_pkcs8(&der)
        .map_err(|e| AsrError::ConnectionFailed(format!("Invalid RSA private key: {e}")))?;

    // Build JWT
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs();

    let header = serde_json::json!({"alg": "RS256", "typ": "JWT"});
    let claims = serde_json::json!({
        "iss": client_email,
        "scope": "https://www.googleapis.com/auth/cloud-platform",
        "aud": token_uri,
        "iat": now,
        "exp": now + 3600,
    });

    let encoded_header = URL_SAFE_NO_PAD.encode(header.to_string().as_bytes());
    let encoded_claims = URL_SAFE_NO_PAD.encode(claims.to_string().as_bytes());
    let signing_input = format!("{}.{}", encoded_header, encoded_claims);

    // Sign with RS256
    let rng = SystemRandom::new();
    let mut signature = vec![0u8; key_pair.public().modulus_len()];
    key_pair
        .sign(
            &RSA_PKCS1_SHA256,
            &rng,
            signing_input.as_bytes(),
            &mut signature,
        )
        .map_err(|e| AsrError::ConnectionFailed(format!("JWT signing failed: {e}")))?;

    let encoded_signature = URL_SAFE_NO_PAD.encode(&signature);
    let jwt = format!("{}.{}", signing_input, encoded_signature);

    // Exchange JWT for access token
    log::info!(
        "Google STT: exchanging SA JWT for access token (iss={})",
        client_email
    );
    let http = reqwest::Client::new();
    let resp = http
        .post(token_uri)
        .form(&[
            ("grant_type", "urn:ietf:params:oauth:grant-type:jwt-bearer"),
            ("assertion", &jwt),
        ])
        .send()
        .await
        .map_err(|e| AsrError::ConnectionFailed(format!("Token exchange request failed: {e}")))?;

    let body: serde_json::Value = resp.json().await.map_err(|e| {
        AsrError::ConnectionFailed(format!("Token exchange response parse failed: {e}"))
    })?;

    if let Some(token) = body["access_token"].as_str() {
        log::info!("Google STT: fresh access token obtained");
        Ok(token.to_string())
    } else {
        let err_msg = body["error_description"]
            .as_str()
            .or(body["error"].as_str())
            .unwrap_or("unknown error");
        Err(AsrError::ConnectionFailed(format!(
            "Token exchange failed: {err_msg}"
        )))
    }
}

/// Google Cloud Speech-to-Text V2 streaming client.
pub struct GoogleSttClient {
    config: AsrConfig,
}

impl GoogleSttClient {
    pub fn new(config: AsrConfig) -> Self {
        Self { config }
    }

    /// Run a streaming recognition session, mirroring the signature of `AsrClient::stream_session`.
    pub async fn stream_session<F>(
        &self,
        sample_rate: u32,
        channels: u16,
        audio_rx: Receiver<Vec<u8>>,
        cancel: tokio_util::sync::CancellationToken,
        _history: Vec<String>,
        on_event: F,
    ) -> Result<(), AsrError>
    where
        F: Fn(AsrEvent) + Send + Sync + 'static,
    {
        if !self.config.is_valid() {
            return Err(AsrError::ConnectionFailed(
                "Invalid Google STT configuration: Project ID and Service Account Key are required"
                    .to_string(),
            ));
        }

        let stream_rate: u32 = if sample_rate != 16_000 {
            16_000
        } else {
            sample_rate
        };

        let location = &self.config.google_location;
        let language_codes = parse_google_language_codes(&self.config.google_language_code);

        log::info!(
            "Google STT connecting (capture {} Hz -> stream {} Hz, {} ch, langs={:?}, location={})",
            sample_rate,
            stream_rate,
            channels,
            language_codes,
            location,
        );

        let t0 = std::time::Instant::now();

        // Obtain an OAuth2 access token (cached for ~50 min)
        let access_token = get_service_account_token(&self.config.google_api_key).await?;
        log::info!("Google STT: token obtained in {:?}", t0.elapsed());

        // Get or create gRPC channel (cached by endpoint URL, reuses HTTP/2 connection)
        let endpoint_url = format!("https://{location}-speech.googleapis.com");
        let channel = get_or_create_channel(&endpoint_url).await?;
        log::info!(
            "Google STT: channel ready in {:?} ({})",
            t0.elapsed(),
            endpoint_url,
        );

        // Attach Bearer token via interceptor
        let mut client =
            SpeechClient::with_interceptor(channel, move |mut req: tonic::Request<()>| {
                let val: MetadataValue<_> = format!("Bearer {}", access_token)
                    .parse()
                    .map_err(|_| tonic::Status::unauthenticated("Invalid access token"))?;
                req.metadata_mut().insert("authorization", val);
                Ok(req)
            });

        // Build the first config message
        let recognizer = format!(
            "projects/{}/locations/{}/recognizers/_",
            self.config.google_project_id, location,
        );

        // Build speech adaptation (phrase hints) from hotwords dictionary
        let phrase_boost = self.config.google_phrase_boost.max(0.0);
        let adaptation = if self.config.hotwords.is_empty() {
            None
        } else {
            let phrases: Vec<google::cloud::speech::v2::phrase_set::Phrase> = self
                .config
                .hotwords
                .iter()
                .take(1000) // Google STT V2 supports up to 1,000 phrases per PhraseSet
                .map(|word| google::cloud::speech::v2::phrase_set::Phrase {
                    value: word.clone(),
                    boost: 0.0,
                })
                .collect();
            log::info!(
                "Google STT: configuring {} phrase hints from hotwords dictionary (boost={})",
                phrases.len(),
                phrase_boost
            );
            Some(SpeechAdaptation {
                phrase_sets: vec![
                    google::cloud::speech::v2::speech_adaptation::AdaptationPhraseSet {
                        value: Some(
                            google::cloud::speech::v2::speech_adaptation::adaptation_phrase_set::Value::InlinePhraseSet(
                                PhraseSet {
                                    name: String::new(),
                                    uid: String::new(),
                                    phrases,
                                    boost: phrase_boost,
                                    display_name: String::new(),
                                    state: 0,
                                    create_time: None,
                                    update_time: None,
                                    delete_time: None,
                                    expire_time: None,
                                    annotations: Default::default(),
                                    etag: String::new(),
                                    reconciling: false,
                                    kms_key_name: String::new(),
                                    kms_key_version_name: String::new(),
                                },
                            ),
                        ),
                    },
                ],
                custom_classes: vec![],
            })
        };

        let config_msg = StreamingRecognizeRequest {
            recognizer: recognizer.clone(),
            streaming_request: Some(StreamingRequest::StreamingConfig(
                StreamingRecognitionConfig {
                    config: Some(RecognitionConfig {
                        model: "chirp_3".to_string(),
                        language_codes: language_codes.clone(),
                        features: None,
                        adaptation,
                        transcript_normalization: None,
                        translation_config: None,
                        denoiser_config: None,
                        decoding_config: Some(
                            google::cloud::speech::v2::recognition_config::DecodingConfig::ExplicitDecodingConfig(
                                ExplicitDecodingConfig {
                                    encoding: google::cloud::speech::v2::explicit_decoding_config::AudioEncoding::Linear16
                                        as i32,
                                    sample_rate_hertz: stream_rate as i32,
                                    audio_channel_count: channels as i32,
                                },
                            ),
                        ),
                    }),
                    config_mask: Some(prost_types::FieldMask {
                        paths: vec!["*".to_string()],
                    }),
                    streaming_features: Some(StreamingRecognitionFeatures {
                        enable_voice_activity_events: false,
                        interim_results: true,
                        voice_activity_timeout: None,
                        // 1=STANDARD, 2=SUPERSHORT, 3=SHORT
                        endpointing_sensitivity: match self.config.google_endpointing.as_str() {
                            "standard" => 1,
                            "short" => 3,
                            _ => 2, // supershort (default)
                        },
                    }),
                },
            )),
        };

        // Set up a channel to feed the request stream.
        // Capacity 256 to buffer audio while the gRPC connection is being established.
        let (tx, rx) = tokio::sync::mpsc::channel::<StreamingRecognizeRequest>(256);

        // Send config message first
        tx.send(config_msg)
            .await
            .map_err(|e| AsrError::ConnectionFailed(format!("Failed to send config: {e}")))?;

        // Spawn the writer task BEFORE calling streaming_recognize().
        // This way audio chunks are pre-buffered in `tx` while the gRPC connection
        // is being established, so config + audio arrive together on the server
        // with no gap (prevents server-side idle timeout).
        let resample_needed = sample_rate != stream_rate;
        if resample_needed {
            log::debug!(
                "Google STT resampling input {} Hz -> {} Hz",
                sample_rate,
                stream_rate
            );
        }

        let cancel_writer = cancel.clone();
        let writer_handle = tokio::spawn(async move {
            let mut audio_rx = audio_rx;
            let mut chunk_count: u32 = 0;
            while let Some(chunk) = tokio::select! {
                _ = cancel_writer.cancelled() => None,
                v = audio_rx.recv() => v,
            } {
                let pcm = if resample_needed {
                    resample_to_16k(&chunk, sample_rate)
                } else {
                    chunk
                };

                let audio_msg = StreamingRecognizeRequest {
                    recognizer: String::new(),
                    streaming_request: Some(StreamingRequest::Audio(pcm)),
                };
                if tx.send(audio_msg).await.is_err() {
                    log::warn!(
                        "Google STT request channel closed (sent {} chunks)",
                        chunk_count
                    );
                    return;
                }
                chunk_count += 1;
            }
            log::debug!(
                "Google STT writer finished ({} audio chunks queued)",
                chunk_count
            );
            // tx is dropped here, signalling end of request stream
        });

        // Start the bidirectional stream.
        // The request_stream will drain config + any already-buffered audio chunks.
        let request_stream = ReceiverStream::new(rx);
        log::debug!("Google STT initiating StreamingRecognize RPC...");
        let response = client
            .streaming_recognize(request_stream)
            .await
            .map_err(|e| {
                AsrError::ConnectionFailed(format!(
                    "StreamingRecognize RPC failed (code={}, source={:?}): {}",
                    e.code(),
                    e.source(),
                    e.message()
                ))
            })?;
        log::info!("Google STT StreamingRecognize stream established");

        let mut resp_stream = response.into_inner();

        let on_event = Arc::new(on_event);

        // Read server responses until stream ends.
        // Google STT sends one is_final per utterance/sentence, so we accumulate
        // across finals to present cumulative text (matching Volcengine behavior).
        // We also track the last partial segment so that when the stream ends,
        // any un-finalized audio is flushed as a final event.
        let mut accumulated_final = String::new();
        let mut pending_partial = String::new();
        loop {
            let msg = tokio::select! {
                _ = cancel.cancelled() => break,
                result = resp_stream.message() => result,
            };
            match msg {
                Ok(Some(resp)) => {
                    process_response(
                        &resp,
                        &on_event,
                        &mut accumulated_final,
                        &mut pending_partial,
                    );
                }
                Ok(None) => {
                    log::debug!("Google STT stream ended (server closed)");
                    // Flush any un-finalized partial as a final event so the
                    // last few words aren't lost when the user stops recording.
                    if !pending_partial.is_empty() {
                        let flushed = format!("{}{}", accumulated_final, pending_partial);
                        log::info!(
                            "Google STT: flushing pending partial as final (partial_len={}, total_len={})",
                            pending_partial.chars().count(),
                            flushed.chars().count(),
                        );
                        on_event(AsrEvent {
                            text: flushed,
                            is_final: true,
                            prefetch: false,
                            definite: true,
                            confidence: None,
                        });
                    }
                    break;
                }
                Err(e) => {
                    log::warn!("Google STT read error: {}", e);
                    break;
                }
            }
        }

        // Wait for writer to finish
        match tokio::time::timeout(Duration::from_millis(2000), writer_handle).await {
            Ok(_) => log::debug!("Google STT writer task completed"),
            Err(_) => log::debug!("Google STT writer task timed out (already done or cancelled)"),
        }

        Ok(())
    }
}

/// Map a Google StreamingRecognizeResponse to AsrEvent(s) and invoke the callback.
///
/// `accumulated_final` tracks text from all previous is_final segments so that
/// each emitted event contains the full cumulative transcript (matching the
/// behavior callers expect from Volcengine's single-final model).
///
/// `pending_partial` tracks the latest interim segment text so that when the
/// stream ends, any un-finalized words can be flushed as a final event.
fn process_response<F>(
    resp: &StreamingRecognizeResponse,
    on_event: &Arc<F>,
    accumulated_final: &mut String,
    pending_partial: &mut String,
) where
    F: Fn(AsrEvent),
{
    for result in &resp.results {
        if let Some(alt) = result.alternatives.first() {
            if alt.transcript.is_empty() {
                continue;
            }
            let confidence = if alt.confidence > 0.0 {
                Some(alt.confidence)
            } else {
                None
            };

            if result.is_final {
                // Append this segment to the accumulated text
                accumulated_final.push_str(&alt.transcript);
                pending_partial.clear(); // this segment is now finalized
                on_event(AsrEvent {
                    text: accumulated_final.clone(),
                    is_final: true,
                    prefetch: false,
                    definite: true,
                    confidence,
                });
            } else {
                // Interim: show accumulated finals + current partial segment
                *pending_partial = alt.transcript.clone();
                let combined = format!("{}{}", accumulated_final, alt.transcript);
                on_event(AsrEvent {
                    text: combined,
                    is_final: false,
                    prefetch: false,
                    definite: false,
                    confidence,
                });
            }
        }
    }
}

fn parse_google_language_codes(raw: &str) -> Vec<String> {
    let mut codes = Vec::new();

    for part in raw.split(|c: char| matches!(c, ',' | ';' | '\n')) {
        let code = part.trim();
        if code.is_empty() {
            continue;
        }

        if code.eq_ignore_ascii_case("auto") {
            return vec!["auto".to_string()];
        }

        if codes
            .iter()
            .any(|existing: &String| existing.eq_ignore_ascii_case(code))
        {
            continue;
        }

        codes.push(code.to_string());
        if codes.len() == 3 {
            break;
        }
    }

    if codes.is_empty() {
        vec!["cmn-Hans-CN".to_string(), "en-US".to_string()]
    } else {
        codes
    }
}

#[cfg(test)]
mod tests {
    use super::parse_google_language_codes;

    #[test]
    fn parse_google_language_codes_supports_bilingual_input() {
        assert_eq!(
            parse_google_language_codes("cmn-Hans-CN, en-US"),
            vec!["cmn-Hans-CN".to_string(), "en-US".to_string()]
        );
    }

    #[test]
    fn parse_google_language_codes_supports_auto() {
        assert_eq!(
            parse_google_language_codes("auto"),
            vec!["auto".to_string()]
        );
        assert_eq!(
            parse_google_language_codes("cmn-Hans-CN, auto, en-US"),
            vec!["auto".to_string()]
        );
    }

    #[test]
    fn parse_google_language_codes_limits_to_three_unique_codes() {
        assert_eq!(
            parse_google_language_codes("cmn-Hans-CN, en-US, ja-JP, en-US, ko-KR"),
            vec![
                "cmn-Hans-CN".to_string(),
                "en-US".to_string(),
                "ja-JP".to_string(),
            ]
        );
    }

    #[test]
    fn parse_google_language_codes_falls_back_to_bilingual_default() {
        assert_eq!(
            parse_google_language_codes(""),
            vec!["cmn-Hans-CN".to_string(), "en-US".to_string()]
        );
    }
}
