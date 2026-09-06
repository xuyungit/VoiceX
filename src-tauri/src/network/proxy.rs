use reqwest::Url;
use std::{collections::HashMap, net::IpAddr};

#[derive(Default)]
pub(super) struct Settings {
    pub proxies: HashMap<String, String>,
    pub bypass: Vec<String>,
    pub exclude_simple: bool,
    pub automatic: bool,
}

// An explicit environment setting overrides the system setting for that scheme.
// Read on every connection so changing the OS proxy does not require a restart.
pub(super) fn resolve(url: &Url) -> Result<Option<Url>, String> {
    resolve_with(
        url,
        |key| std::env::var(key).ok(),
        || platform_settings(url),
    )
}

fn resolve_with(
    url: &Url,
    env: impl Fn(&str) -> Option<String>,
    system: impl FnOnce() -> Result<Settings, String>,
) -> Result<Option<Url>, String> {
    let host = url
        .host_str()
        .ok_or("WebSocket URL has no host")?
        .trim_matches(['[', ']']);
    // Local diagnostic servers must remain local, including IPv6 loopback.
    if host.eq_ignore_ascii_case("localhost")
        || host.parse::<IpAddr>().is_ok_and(|ip| ip.is_loopback())
    {
        return Ok(None);
    }
    let read = |lower: &str, upper: &str| {
        env(lower)
            .or_else(|| env(upper))
            .filter(|s| !s.trim().is_empty())
    };
    if let Some(bypass) = read("no_proxy", "NO_PROXY") {
        if bypass
            .split(',')
            .any(|rule| bypass_matches(url, rule.trim(), true))
        {
            return Ok(None);
        }
    }
    let scheme = match url.scheme() {
        "wss" => "https",
        "ws" => "http",
        _ => return Err("Expected ws or wss URL".into()),
    };
    let specific = if scheme == "https" {
        read("https_proxy", "HTTPS_PROXY")
    } else {
        env("http_proxy")
            .or_else(|| {
                if env("REQUEST_METHOD").is_none() {
                    env("HTTP_PROXY")
                } else {
                    None
                }
            })
            .filter(|s| !s.trim().is_empty())
    };
    if let Some(proxy) = specific.or_else(|| read("all_proxy", "ALL_PROXY")) {
        return parse_proxy(&proxy).map(Some);
    }
    let settings = system()?;
    if (settings.exclude_simple && !host.contains('.') && host.parse::<IpAddr>().is_err())
        || settings
            .bypass
            .iter()
            .any(|rule| bypass_matches(url, rule, false))
    {
        return Ok(None);
    }
    if settings.automatic {
        return Err("系统启用了自动代理发现，但未提供 PAC 地址；请配置 PAC 地址或手动代理 / WPAD discovery without a PAC URL is not supported on this platform".into());
    }
    settings
        .proxies
        .get(scheme)
        .or_else(|| settings.proxies.get("socks"))
        .or_else(|| settings.proxies.get("all"))
        .map(|value| parse_proxy(value))
        .transpose()
}

fn parse_proxy(value: &str) -> Result<Url, String> {
    let value = value.trim();
    let url = if value.contains("://") {
        Url::parse(value)
    } else {
        Url::parse(&format!("http://{value}"))
    }
    .map_err(|_| "Invalid proxy address / 代理地址格式无效".to_string())?;
    if url.host_str().is_none()
        || !matches!(url.scheme(), "http" | "https" | "socks5" | "socks5h")
        || url.query().is_some()
        || url.fragment().is_some()
        || url.path() != "/" && !url.path().is_empty()
    {
        return Err(
            "Unsupported proxy configuration; use HTTP, HTTPS or SOCKS5 / 不支持的代理配置".into(),
        );
    }
    Ok(url)
}

fn bypass_matches(url: &Url, rule: &str, subdomains: bool) -> bool {
    let host = url
        .host_str()
        .unwrap_or_default()
        .trim_matches(['[', ']'])
        .to_ascii_lowercase();
    let rule = rule.trim().to_ascii_lowercase();
    if rule.is_empty() {
        return false;
    }
    if rule == "<local>" {
        return !host.contains('.') && host.parse::<IpAddr>().is_err();
    }
    if let Ok(network) = rule.parse::<ipnet::IpNet>() {
        return host.parse::<IpAddr>().is_ok_and(|ip| network.contains(&ip));
    }
    if rule.trim_matches(['[', ']']) == host {
        return true;
    }
    let (pattern, port) = if rule.starts_with('[') {
        match rule.find(']') {
            Some(end) => (&rule[1..end], rule[end + 1..].strip_prefix(':')),
            None => return false,
        }
    } else if rule.matches(':').count() == 1 {
        let (pattern, port) = rule.rsplit_once(':').unwrap();
        (pattern, Some(port))
    } else {
        (rule.as_str(), None)
    };
    if let Some(port) = port {
        let actual = url
            .port()
            .unwrap_or(if url.scheme() == "wss" { 443 } else { 80 });
        if port.parse::<u16>().ok() != Some(actual) {
            return false;
        }
    }
    if pattern.starts_with('.') {
        return host == &pattern[1..] || host.ends_with(pattern);
    }
    glob_matches(pattern.as_bytes(), host.as_bytes())
        || subdomains && !pattern.contains('*') && host.ends_with(&format!(".{pattern}"))
}

fn glob_matches(pattern: &[u8], text: &[u8]) -> bool {
    let (mut p, mut t, mut star, mut retry) = (0, 0, None, 0);
    while t < text.len() {
        if p < pattern.len() && pattern[p] == text[t] {
            p += 1;
            t += 1;
        } else if p < pattern.len() && pattern[p] == b'*' {
            star = Some(p);
            p += 1;
            retry = t;
        } else if let Some(at) = star {
            retry += 1;
            t = retry;
            p = at + 1;
        } else {
            return false;
        }
    }
    while p < pattern.len() && pattern[p] == b'*' {
        p += 1;
    }
    p == pattern.len()
}

#[cfg(target_os = "macos")]
fn platform_settings(url: &Url) -> Result<Settings, String> {
    use system_configuration::{
        core_foundation::{
            array::CFArray,
            base::{CFType, TCFType},
            number::CFNumber,
            string::CFString,
        },
        dynamic_store::SCDynamicStoreBuilder,
    };
    let store = SCDynamicStoreBuilder::new("VoiceX network proxy").build();
    let map = store
        .get_proxies()
        .ok_or("Cannot read macOS system proxy settings")?;
    let number = |key: &str| {
        map.find(&CFString::new(key))
            .and_then(|v| v.downcast::<CFNumber>())
            .and_then(|v| v.to_i64())
    };
    let string = |key: &str| {
        map.find(&CFString::new(key))
            .and_then(|v| v.downcast::<CFString>())
            .map(|v| v.to_string())
    };
    if number("ProxyAutoConfigEnable") == Some(1) {
        let pac = string("ProxyAutoConfigURLString").ok_or("Enabled PAC proxy has no URL")?;
        return super::mac_pac::resolve(&pac, url);
    }
    let mut settings = Settings {
        exclude_simple: number("ExcludeSimpleHostnames") == Some(1),
        automatic: number("ProxyAutoConfigEnable") == Some(1)
            || number("ProxyAutoDiscoveryEnable") == Some(1),
        ..Settings::default()
    };
    for (prefix, scheme, transport) in [
        ("HTTP", "http", "http"),
        ("HTTPS", "https", "http"),
        ("SOCKS", "socks", "socks5h"),
    ] {
        if number(&format!("{prefix}Enable")) == Some(1) {
            let host =
                string(&format!("{prefix}Proxy")).ok_or("Enabled system proxy has no host")?;
            let port = number(&format!("{prefix}Port"))
                .filter(|port| (1..=65535).contains(port))
                .ok_or("Enabled system proxy has invalid port")?;
            let host = if host.contains(':') && !host.starts_with('[') {
                format!("[{host}]")
            } else {
                host
            };
            settings
                .proxies
                .insert(scheme.into(), format!("{transport}://{host}:{port}"));
        }
    }
    if let Some(values) = map
        .find(&CFString::new("ExceptionsList"))
        .and_then(|v| v.downcast::<CFArray>())
    {
        let values =
            unsafe { CFArray::<CFType>::wrap_under_get_rule(values.as_concrete_TypeRef()) };
        for value in values.iter() {
            if let Some(value) = value.downcast::<CFString>() {
                settings.bypass.push(value.to_string());
            }
        }
    }
    Ok(settings)
}

#[cfg(target_os = "windows")]
fn platform_settings(url: &Url) -> Result<Settings, String> {
    use windows_sys::Win32::{Foundation::GlobalFree, Networking::WinHttp::*};
    // Read the interactive user's Windows proxy, including the actual
    // auto-detect flag (which is not reliably stored in an AutoDetect value).
    unsafe {
        let mut config: WINHTTP_CURRENT_USER_IE_PROXY_CONFIG = std::mem::zeroed();
        if WinHttpGetIEProxyConfigForCurrentUser(&mut config) == 0 {
            return Err(format!(
                "Cannot read Windows system proxy: {}",
                std::io::Error::last_os_error()
            ));
        }
        unsafe fn take_string(pointer: *mut u16) -> String {
            if pointer.is_null() {
                return String::new();
            }
            let mut len = 0;
            while *pointer.add(len) != 0 {
                len += 1;
            }
            let value = String::from_utf16_lossy(std::slice::from_raw_parts(pointer, len));
            GlobalFree(pointer.cast());
            value
        }
        let pac = take_string(config.lpszAutoConfigUrl);
        let server = take_string(config.lpszProxy);
        let bypass = take_string(config.lpszProxyBypass);
        if !pac.is_empty() || config.fAutoDetect != 0 {
            let agent: Vec<u16> = "VoiceX\0".encode_utf16().collect();
            let session = WinHttpOpen(
                agent.as_ptr(),
                WINHTTP_ACCESS_TYPE_NO_PROXY,
                std::ptr::null(),
                std::ptr::null(),
                0,
            );
            if session.is_null() {
                return Err("Cannot initialize Windows automatic proxy resolver".into());
            }
            let mut target = url.clone();
            let _ = target.set_scheme(if url.scheme() == "wss" {
                "https"
            } else {
                "http"
            });
            target.set_query(None);
            target.set_fragment(None);
            if target.scheme() == "https" {
                target.set_path("/");
            }
            let target: Vec<u16> = target.as_str().encode_utf16().chain(Some(0)).collect();
            let pac_wide: Vec<u16> = pac.encode_utf16().chain(Some(0)).collect();
            let mut options: WINHTTP_AUTOPROXY_OPTIONS = std::mem::zeroed();
            if !pac.is_empty() {
                options.dwFlags = WINHTTP_AUTOPROXY_CONFIG_URL;
                options.lpszAutoConfigUrl = pac_wide.as_ptr();
            } else {
                options.dwFlags = WINHTTP_AUTOPROXY_AUTO_DETECT;
                options.dwAutoDetectFlags =
                    WINHTTP_AUTO_DETECT_TYPE_DHCP | WINHTTP_AUTO_DETECT_TYPE_DNS_A;
            }
            if WinHttpSetTimeouts(session, 3000, 3000, 3000, 3000) == 0 {
                WinHttpCloseHandle(session);
                return Err("Cannot set Windows proxy resolution timeout".into());
            }
            let mut info: WINHTTP_PROXY_INFO = std::mem::zeroed();
            let success =
                WinHttpGetProxyForUrl(session, target.as_ptr(), &mut options, &mut info) != 0;
            let error = std::io::Error::last_os_error();
            WinHttpCloseHandle(session);
            let resolved = take_string(info.lpszProxy);
            let bypass = take_string(info.lpszProxyBypass);
            if success {
                if info.dwAccessType == WINHTTP_ACCESS_TYPE_NO_PROXY {
                    return Ok(Settings::default());
                }
                // WinHTTP returns proxies in preference order. A failed first
                // proxy is reported rather than silently bypassing the policy.
                let first = resolved.split(';').next().unwrap_or_default().trim();
                let mut settings = Settings::default();
                settings
                    .proxies
                    .insert("all".into(), parse_proxy(first)?.to_string());
                settings.bypass = bypass.split(';').map(str::to_string).collect();
                return Ok(settings);
            }
            if !pac.is_empty()
                || error.raw_os_error() != Some(ERROR_WINHTTP_AUTODETECTION_FAILED as i32)
            {
                return Err(format!(
                    "Windows automatic proxy resolution failed: {error}"
                ));
            }
            // WPAD found no configuration: Windows' explicit static settings
            // (or DIRECT when none exist) are the next configured policy layer.
            log::debug!("Windows WPAD has no configuration; using static system proxy settings");
        }
        Ok(Settings {
            automatic: false,
            proxies: if server.trim().is_empty() {
                HashMap::new()
            } else {
                parse_windows_servers(&server)?
            },
            bypass: bypass
                .split(';')
                .map(|value| value.trim().to_string())
                .collect(),
            ..Settings::default()
        })
    }
}

#[cfg(any(target_os = "windows", test))]
fn parse_windows_servers(value: &str) -> Result<HashMap<String, String>, String> {
    let mut proxies = HashMap::new();
    for entry in value.split(';').map(str::trim).filter(|s| !s.is_empty()) {
        let (scheme, address) = entry.split_once('=').unwrap_or(("all", entry));
        let scheme = scheme.trim().to_ascii_lowercase();
        let address = address.trim();
        let address = if scheme == "socks" && !address.contains("://") {
            format!("socks5h://{address}")
        } else {
            address.into()
        };
        parse_proxy(&address)?;
        proxies.insert(scheme, address);
    }
    if proxies.is_empty() {
        return Err("Enabled system proxy is empty".into());
    }
    Ok(proxies)
}

#[cfg(not(any(target_os = "macos", target_os = "windows")))]
fn platform_settings(_url: &Url) -> Result<Settings, String> {
    Ok(Settings::default())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn environment_precedence_and_bypass_do_not_need_system_lookup() {
        let url = Url::parse("wss://api.openai.com/v1/realtime").unwrap();
        let env = |key: &str| match key {
            "https_proxy" => Some("http://user:CaseSensitive@proxy:8080".into()),
            "all_proxy" => Some("socks5://other:1080".into()),
            _ => None,
        };
        let proxy = resolve_with(&url, env, || panic!("environment overrides OS"))
            .unwrap()
            .unwrap();
        assert_eq!(proxy.password(), Some("CaseSensitive"));
        assert_eq!(proxy.host_str(), Some("proxy"));
        assert!(resolve_with(
            &url,
            |key| (key == "NO_PROXY").then(|| ".openai.com".into()),
            || panic!("bypass overrides OS")
        )
        .unwrap()
        .is_none());
    }
    #[test]
    fn bypass_handles_ports_subnets_ipv6_and_wildcards() {
        for (url, rule, expected) in [
            ("wss://api.openai.com/", "*.openai.com", true),
            ("wss://api.openai.com.evil/", "*.openai.com", false),
            ("ws://192.168.1.7/", "192.168.*", true),
            ("ws://192.168.1.7/", "192.168.0.0/16", true),
            ("wss://[::1]:8443/", "[::1]:8443", true),
            ("wss://api.openai.com/", "api.openai.com:80", false),
            ("wss://api.openai.com/", "api.openai.com:443", true),
        ] {
            assert_eq!(
                bypass_matches(&Url::parse(url).unwrap(), rule, false),
                expected
            );
        }
    }
    #[test]
    fn windows_protocol_rules_and_pac_failure_are_explicit() {
        let proxies =
            parse_windows_servers("http=proxy:8080;https=secure:8081;socks=sock:1080").unwrap();
        let url = Url::parse("wss://api.openai.com/").unwrap();
        assert_eq!(
            resolve_with(
                &url,
                |_| None,
                || Ok(Settings {
                    proxies,
                    ..Settings::default()
                })
            )
            .unwrap()
            .unwrap()
            .host_str(),
            Some("secure")
        );
        assert!(resolve_with(
            &url,
            |_| None,
            || Ok(Settings {
                automatic: true,
                ..Settings::default()
            })
        )
        .is_err());
        assert!(
            parse_proxy("http://user:secret@host:badport")
                .unwrap_err()
                .contains("secret")
                == false
        );
    }
}
