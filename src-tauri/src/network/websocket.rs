use base64::{engine::general_purpose::STANDARD, Engine};
use reqwest::Url;
use std::{io, time::Duration};
use tokio::{
    io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt},
    net::TcpStream,
};
use tokio_tungstenite::{
    client_async_tls,
    tungstenite::{
        client::IntoClientRequest,
        handshake::client::{Request, Response},
        Error,
    },
    MaybeTlsStream, WebSocketStream,
};

pub(crate) trait Transport: AsyncRead + AsyncWrite + Unpin + Send {}
impl<T: AsyncRead + AsyncWrite + Unpin + Send> Transport for T {}
type Socket = Box<dyn Transport>;
pub(crate) type WsStream = WebSocketStream<MaybeTlsStream<Socket>>;

/// Resolve each new connection independently. Never retry a failed proxy by
/// connecting directly: doing so would violate the selected network route.
pub(crate) async fn connect_async<R: IntoClientRequest>(
    request: R,
) -> Result<(WsStream, Response), Error> {
    let request = request.into_client_request()?;
    tokio::time::timeout(Duration::from_secs(20), async {
        let url =
            Url::parse(&request.uri().to_string()).map_err(|_| failure("Invalid WebSocket URL"))?;
        let target = url.clone();
        let proxy = tokio::task::spawn_blocking(move || super::proxy::resolve(&target))
            .await
            .map_err(|_| failure("System proxy lookup failed"))?
            .map_err(failure)?;
        connect_routed(request, url, proxy).await
    })
    .await
    .map_err(|_| {
        Error::Io(io::Error::new(
            io::ErrorKind::TimedOut,
            "WebSocket connection/proxy handshake timed out",
        ))
    })?
}

fn failure(message: impl Into<String>) -> Error {
    Error::Io(io::Error::new(io::ErrorKind::Other, message.into()))
}

async fn connect_routed(
    request: Request,
    url: Url,
    proxy: Option<Url>,
) -> Result<(WsStream, Response), Error> {
    let host = url
        .host_str()
        .ok_or_else(|| failure("WebSocket host is missing"))?
        .trim_matches(['[', ']']);
    let port = url
        .port()
        .unwrap_or(if url.scheme() == "wss" { 443 } else { 80 });
    let socket: Socket = match proxy {
        None => Box::new(TcpStream::connect((host, port)).await?),
        Some(proxy) => {
            let proxy_host = proxy
                .host_str()
                .ok_or_else(|| failure("Proxy host is missing"))?
                .trim_matches(['[', ']']);
            let proxy_port = proxy.port_or_known_default().unwrap_or(1080);
            let username = urlencoding::decode(proxy.username())
                .map_err(|_| failure("Invalid proxy username encoding"))?;
            let password = urlencoding::decode(proxy.password().unwrap_or_default())
                .map_err(|_| failure("Invalid proxy password encoding"))?;
            log::debug!("WebSocket routing through {} proxy", proxy.scheme());
            if matches!(proxy.scheme(), "socks5" | "socks5h") {
                let target = if proxy.scheme() == "socks5h" {
                    tokio_socks::TargetAddr::Domain(host.into(), port)
                } else {
                    tokio_socks::TargetAddr::Ip(
                        tokio::net::lookup_host((host, port))
                            .await?
                            .next()
                            .ok_or_else(|| failure("SOCKS target DNS returned no address"))?,
                    )
                };
                let stream = if username.is_empty() && password.is_empty() {
                    tokio_socks::tcp::Socks5Stream::connect((proxy_host, proxy_port), target).await
                } else {
                    tokio_socks::tcp::Socks5Stream::connect_with_password(
                        (proxy_host, proxy_port),
                        target,
                        &username,
                        &password,
                    )
                    .await
                }
                .map_err(|e| failure(format!("SOCKS proxy connection failed: {e}")))?;
                Box::new(stream.into_inner())
            } else {
                let tcp = TcpStream::connect((proxy_host, proxy_port))
                    .await
                    .map_err(|e| failure(format!("Cannot connect to system HTTP proxy: {e}")))?;
                let mut tunnel: Socket = if proxy.scheme() == "https" {
                    let tls = native_tls::TlsConnector::new()
                        .map_err(|e| failure(format!("Proxy TLS initialization failed: {e}")))?;
                    Box::new(
                        tokio_native_tls::TlsConnector::from(tls)
                            .connect(proxy_host, tcp)
                            .await
                            .map_err(|e| failure(format!("Proxy TLS verification failed: {e}")))?,
                    )
                } else {
                    Box::new(tcp)
                };
                let auth = if username.is_empty() && password.is_empty() {
                    None
                } else {
                    Some(STANDARD.encode(format!("{username}:{password}")))
                };
                http_connect(&mut tunnel, host, port, auth.as_deref()).await?;
                tunnel
            }
        }
    };
    // TLS still authenticates the origin hostname after CONNECT/SOCKS. No
    // certificate-validation override and no provider headers sent to CONNECT.
    client_async_tls(request, socket).await
}

async fn http_connect(
    socket: &mut Socket,
    host: &str,
    port: u16,
    auth: Option<&str>,
) -> Result<(), Error> {
    let authority = if host.contains(':') {
        format!("[{host}]:{port}")
    } else {
        format!("{host}:{port}")
    };
    let mut request = format!("CONNECT {authority} HTTP/1.1\r\nHost: {authority}\r\n");
    if let Some(auth) = auth {
        request.push_str(&format!("Proxy-Authorization: Basic {auth}\r\n"));
    }
    request.push_str("\r\n");
    socket.write_all(request.as_bytes()).await?;
    socket.flush().await?;
    let mut headers = Vec::new();
    while !headers.ends_with(b"\r\n\r\n") {
        if headers.len() >= 16 * 1024 {
            return Err(failure("Proxy CONNECT response headers are too large"));
        }
        headers.push(socket.read_u8().await?);
    }
    let status = std::str::from_utf8(&headers)
        .ok()
        .and_then(|text| text.lines().next())
        .and_then(|line| {
            let mut fields = line.split_whitespace();
            let version = fields.next()?;
            if !matches!(version, "HTTP/1.0" | "HTTP/1.1") {
                return None;
            }
            fields.next()?.parse::<u16>().ok()
        })
        .ok_or_else(|| failure("Invalid proxy CONNECT response"))?;
    if !(200..300).contains(&status) {
        return Err(failure(if status == 407 {
            "Proxy authentication required (407) / 系统代理要求认证".into()
        } else {
            format!("System proxy rejected CONNECT (HTTP {status}) / 系统代理拒绝连接")
        }));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use futures_util::{SinkExt, StreamExt};
    use tokio::net::TcpListener;
    use tokio_tungstenite::{accept_async, tungstenite::Message};

    #[tokio::test]
    async fn connect_tunnels_remote_hostname_then_upgrades_without_leaking_authorization() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let proxy = Url::parse(&format!("http://{}", listener.local_addr().unwrap())).unwrap();
        let server = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            let mut header = Vec::new();
            while !header.ends_with(b"\r\n\r\n") {
                header.push(stream.read_u8().await.unwrap());
            }
            let header = String::from_utf8(header).unwrap();
            assert!(header.starts_with("CONNECT unresolvable.invalid:80 HTTP/1.1"));
            assert!(!header.contains("secret-provider-token"));
            stream
                .write_all(b"HTTP/1.1 200 Connection established\r\n\r\n")
                .await
                .unwrap();
            let mut ws = accept_async(stream).await.unwrap();
            assert_eq!(
                ws.next().await.unwrap().unwrap(),
                Message::Text("probe".into())
            );
            ws.send(Message::Text("ok".into())).await.unwrap();
        });
        let url = Url::parse("ws://unresolvable.invalid/transcription").unwrap();
        let mut request = url.as_str().into_client_request().unwrap();
        request.headers_mut().insert(
            "Authorization",
            "Bearer secret-provider-token".parse().unwrap(),
        );
        let (mut ws, _) = connect_routed(request, url, Some(proxy)).await.unwrap();
        ws.send(Message::Text("probe".into())).await.unwrap();
        assert_eq!(
            ws.next().await.unwrap().unwrap(),
            Message::Text("ok".into())
        );
        server.await.unwrap();
    }

    #[tokio::test]
    async fn rejected_proxy_is_an_error_not_a_direct_retry() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let proxy = Url::parse(&format!("http://{}", listener.local_addr().unwrap())).unwrap();
        let server = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            let mut header = Vec::new();
            while !header.ends_with(b"\r\n\r\n") {
                header.push(stream.read_u8().await.unwrap());
            }
            stream
                .write_all(b"HTTP/1.1 407 Proxy Authentication Required\r\n\r\n")
                .await
                .unwrap();
        });
        let url = Url::parse("wss://unresolvable.invalid/").unwrap();
        let error = connect_routed(
            url.as_str().into_client_request().unwrap(),
            url,
            Some(proxy),
        )
        .await
        .err()
        .unwrap();
        assert!(error.to_string().contains("407"));
        server.await.unwrap();
    }
    #[tokio::test]
    async fn socks5h_sends_hostname_to_proxy_without_local_dns() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let proxy = Url::parse(&format!("socks5h://{}", listener.local_addr().unwrap())).unwrap();
        let server = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            let mut greeting = [0; 3];
            stream.read_exact(&mut greeting).await.unwrap();
            assert_eq!(greeting, [5, 1, 0]);
            stream.write_all(&[5, 0]).await.unwrap();
            let mut header = [0; 5];
            stream.read_exact(&mut header).await.unwrap();
            assert_eq!(&header[..4], &[5, 1, 0, 3]);
            let mut host = vec![0; header[4] as usize];
            stream.read_exact(&mut host).await.unwrap();
            assert_eq!(host, b"unresolvable.invalid");
            assert_eq!(stream.read_u16().await.unwrap(), 80);
            stream
                .write_all(&[5, 0, 0, 1, 127, 0, 0, 1, 0, 80])
                .await
                .unwrap();
            let mut ws = accept_async(stream).await.unwrap();
            ws.send(Message::Text("SOCKS connected".into()))
                .await
                .unwrap();
        });
        let url = Url::parse("ws://unresolvable.invalid/").unwrap();
        let (mut ws, _) = connect_routed(
            url.as_str().into_client_request().unwrap(),
            url,
            Some(proxy),
        )
        .await
        .unwrap();
        assert_eq!(
            ws.next().await.unwrap().unwrap(),
            Message::Text("SOCKS connected".into())
        );
        server.await.unwrap();
    }

    #[tokio::test]
    async fn cancelling_a_pending_proxy_handshake_closes_the_socket() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let proxy = Url::parse(&format!("http://{}", listener.local_addr().unwrap())).unwrap();
        let (ready_tx, ready_rx) = tokio::sync::oneshot::channel();
        let server = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            let mut header = Vec::new();
            while !header.ends_with(b"\r\n\r\n") {
                header.push(stream.read_u8().await.unwrap());
            }
            ready_tx.send(()).unwrap();
            assert_eq!(
                tokio::time::timeout(Duration::from_secs(2), stream.read(&mut [0; 1]))
                    .await
                    .unwrap()
                    .unwrap(),
                0
            );
        });
        let url = Url::parse("wss://unresolvable.invalid/").unwrap();
        tokio::select! {
            _ = ready_rx => {},
            _ = connect_routed(url.as_str().into_client_request().unwrap(), url, Some(proxy)) => panic!("proxy should still be waiting"),
        }
        server.await.unwrap();
    }
}
