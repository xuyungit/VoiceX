//! Resumable, checksum-verified downloads. A source is transport, not identity:
//! the bundled file size and SHA-256 are authoritative across both mirrors.

use futures_util::StreamExt;
use sha2::{Digest, Sha256};
use std::{path::Path, time::Duration};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio_util::sync::CancellationToken;

use super::managed_models::ModelFile;

pub(super) async fn download_file(
    client: &reqwest_updater_proxy::Client,
    url: &str,
    path: &Path,
    spec: &ModelFile,
    cancel: &CancellationToken,
    mut progress: impl FnMut(u64, bool),
) -> Result<(), String> {
    let offset = match tokio::fs::metadata(path).await {
        Ok(metadata) => metadata.len(),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => 0,
        Err(error) => return Err(error.to_string()),
    };
    if offset > spec.bytes {
        return Err(format!(
            "{}: partial file is too large; download again",
            spec.name
        ));
    }
    let mut hash = Sha256::new();
    if offset > 0 {
        let mut file = tokio::fs::File::open(path)
            .await
            .map_err(|e| e.to_string())?;
        let mut buffer = vec![0; 1024 * 1024];
        let mut read = 0;
        while read < offset {
            let count = tokio::select! {
                _ = cancel.cancelled() => return Err("cancelled".into()),
                result = file.read(&mut buffer) => result.map_err(|e| e.to_string())?,
            };
            if count == 0 {
                return Err("Partial file changed while verifying".into());
            }
            hash.update(&buffer[..count]);
            read += count as u64;
            progress(read, true);
        }
    }
    if offset < spec.bytes {
        let mut request = client.get(url);
        if offset > 0 {
            request = request.header("Range", format!("bytes={offset}-"));
        }
        let response = tokio::select! {
            _ = cancel.cancelled() => return Err("cancelled".into()),
            result = request.send() => result.map_err(|e| e.without_url().to_string())?,
        };
        let status = response.status();
        if offset > 0 {
            let expected = format!("bytes {offset}-{}", spec.bytes - 1);
            let range = response
                .headers()
                .get("content-range")
                .and_then(|h| h.to_str().ok());
            if status != 206 || range != Some(format!("{expected}/{}", spec.bytes).as_str()) {
                return Err(format!(
                    "Resume rejected (HTTP {status}); choose another source or download again"
                ));
            }
        } else if status != 200 {
            return Err(format!("Download failed (HTTP {status})"));
        }
        if response
            .content_length()
            .is_some_and(|length| length != spec.bytes - offset)
        {
            return Err(format!("{}: unexpected download size", spec.name));
        }
        let mut file = tokio::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)
            .await
            .map_err(|e| e.to_string())?;
        let mut received = offset;
        let mut stream = response.bytes_stream();
        loop {
            let chunk = tokio::select! {
                _ = cancel.cancelled() => { file.flush().await.map_err(|e| e.to_string())?; return Err("cancelled".into()); },
                result = tokio::time::timeout(Duration::from_secs(30), stream.next()) => result.map_err(|_| "Download stalled for 30 seconds")?,
            };
            let Some(chunk) = chunk else { break };
            let chunk = chunk.map_err(|e| e.without_url().to_string())?;
            if received + chunk.len() as u64 > spec.bytes {
                return Err("Server sent more bytes than expected".into());
            }
            file.write_all(&chunk).await.map_err(|e| e.to_string())?;
            hash.update(&chunk);
            received += chunk.len() as u64;
            progress(received, false);
        }
        file.flush().await.map_err(|e| e.to_string())?;
        file.sync_all().await.map_err(|e| e.to_string())?;
        if received != spec.bytes {
            return Err(format!(
                "{}: download ended early; resume to continue",
                spec.name
            ));
        }
    }
    if hex::encode(hash.finalize()) != spec.sha256 {
        return Err(format!("{}: SHA-256 mismatch; download again", spec.name));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    async fn server(response: &'static str) -> (String, tokio::task::JoinHandle<String>) {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}/model", listener.local_addr().unwrap());
        let task = tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.unwrap();
            let mut bytes = Vec::new();
            while !bytes.windows(4).any(|w| w == b"\r\n\r\n") {
                let mut buffer = [0; 1024];
                let n = socket.read(&mut buffer).await.unwrap();
                assert!(n > 0);
                bytes.extend_from_slice(&buffer[..n]);
            }
            socket.write_all(response.as_bytes()).await.unwrap();
            String::from_utf8(bytes).unwrap()
        });
        (url, task)
    }

    fn spec() -> ModelFile {
        ModelFile {
            name: "weights".into(),
            bytes: 6,
            sha256: hex::encode(Sha256::digest(b"abcdef")),
        }
    }

    #[tokio::test]
    async fn resumes_from_the_existing_length_and_verifies_the_whole_file() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("weights");
        tokio::fs::write(&path, b"abc").await.unwrap();
        let (url, task) = server("HTTP/1.1 206 Partial Content\r\nContent-Length: 3\r\nContent-Range: bytes 3-5/6\r\nConnection: close\r\n\r\ndef").await;
        let client = reqwest_updater_proxy::Client::builder()
            .no_proxy()
            .build()
            .unwrap();
        download_file(
            &client,
            &url,
            &path,
            &spec(),
            &CancellationToken::new(),
            |_, _| {},
        )
        .await
        .unwrap();
        assert!(task
            .await
            .unwrap()
            .to_lowercase()
            .contains("range: bytes=3-"));
        assert_eq!(tokio::fs::read(path).await.unwrap(), b"abcdef");
    }

    #[tokio::test]
    async fn a_server_ignoring_range_cannot_append_a_second_copy() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("weights");
        tokio::fs::write(&path, b"abc").await.unwrap();
        let (url, task) =
            server("HTTP/1.1 200 OK\r\nContent-Length: 6\r\nConnection: close\r\n\r\nabcdef").await;
        let client = reqwest_updater_proxy::Client::builder()
            .no_proxy()
            .build()
            .unwrap();
        assert!(download_file(
            &client,
            &url,
            &path,
            &spec(),
            &CancellationToken::new(),
            |_, _| {}
        )
        .await
        .unwrap_err()
        .contains("Resume rejected"));
        task.await.unwrap();
        assert_eq!(tokio::fs::read(path).await.unwrap(), b"abc");
    }

    #[tokio::test]
    async fn corrupt_complete_files_are_rejected_without_a_network_request() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("weights");
        tokio::fs::write(&path, b"abcdeg").await.unwrap();
        assert!(download_file(
            &reqwest_updater_proxy::Client::new(),
            "http://127.0.0.1:1",
            &path,
            &spec(),
            &CancellationToken::new(),
            |_, _| {}
        )
        .await
        .unwrap_err()
        .contains("SHA-256"));
    }

    #[tokio::test]
    async fn pausing_flushes_partial_bytes_for_a_later_process_to_resume() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("weights");
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}/model", listener.local_addr().unwrap());
        let server = tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.unwrap();
            let mut request = [0; 4096];
            assert!(socket.read(&mut request).await.unwrap() > 0);
            socket
                .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 6\r\n\r\nabc")
                .await
                .unwrap();
            std::future::pending::<()>().await;
        });
        let cancel = CancellationToken::new();
        let stop = cancel.clone();
        let (received, ready) = tokio::sync::oneshot::channel();
        let target = path.clone();
        let download = tokio::spawn(async move {
            let mut received = Some(received);
            let client = reqwest_updater_proxy::Client::builder()
                .no_proxy()
                .build()
                .unwrap();
            download_file(&client, &url, &target, &spec(), &stop, |n, _| {
                if n == 3 {
                    if let Some(sender) = received.take() {
                        sender.send(()).unwrap();
                    }
                }
            })
            .await
        });
        tokio::time::timeout(Duration::from_secs(5), ready)
            .await
            .unwrap()
            .unwrap();
        cancel.cancel();
        assert_eq!(download.await.unwrap().unwrap_err(), "cancelled");
        assert_eq!(tokio::fs::read(path).await.unwrap(), b"abc");
        server.abort();
    }
}
