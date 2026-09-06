//! Use Apple's PAC evaluator. The run-loop source is invalidated before its
//! callback context goes out of scope, including on timeout.
use super::proxy::Settings;
use reqwest::Url;
use std::{
    ffi::c_void,
    ptr,
    time::{Duration, Instant},
};
use system_configuration::core_foundation::{
    array::{CFArray, CFArrayRef},
    base::{CFType, TCFType},
    dictionary::CFDictionary,
    error::CFErrorRef,
    number::CFNumber,
    runloop::*,
    string::{CFString, CFStringRef},
    url::{CFURLCreateWithString, CFURLRef, CFURL},
};

#[repr(C)]
struct Context {
    version: isize,
    info: *mut c_void,
    retain: Option<unsafe extern "C" fn(*mut c_void) -> *mut c_void>,
    release: Option<unsafe extern "C" fn(*mut c_void)>,
    description: Option<unsafe extern "C" fn(*mut c_void) -> CFStringRef>,
}
type Callback = unsafe extern "C" fn(*mut c_void, CFArrayRef, CFErrorRef);
#[link(name = "CFNetwork", kind = "framework")]
extern "C" {
    fn CFNetworkExecuteProxyAutoConfigurationURL(
        pac: CFURLRef,
        target: CFURLRef,
        callback: Callback,
        context: *mut Context,
    ) -> CFRunLoopSourceRef;
    #[cfg(test)]
    fn CFNetworkExecuteProxyAutoConfigurationScript(
        script: CFStringRef,
        target: CFURLRef,
        callback: Callback,
        context: *mut Context,
    ) -> CFRunLoopSourceRef;
    static kCFProxyTypeKey: CFStringRef;
    static kCFProxyTypeNone: CFStringRef;
    static kCFProxyTypeHTTP: CFStringRef;
    static kCFProxyTypeHTTPS: CFStringRef;
    static kCFProxyTypeSOCKS: CFStringRef;
    static kCFProxyHostNameKey: CFStringRef;
    static kCFProxyPortNumberKey: CFStringRef;
}

fn cf_url(url: &str) -> Result<CFURL, String> {
    let text = CFString::new(url);
    let raw =
        unsafe { CFURLCreateWithString(ptr::null(), text.as_concrete_TypeRef(), ptr::null()) };
    if raw.is_null() {
        return Err("Invalid PAC target URL".into());
    }
    Ok(unsafe { CFURL::wrap_under_create_rule(raw) })
}

fn target_url(url: &Url) -> Result<CFURL, String> {
    let mut target = url.clone();
    let _ = target.set_scheme(if url.scheme() == "wss" {
        "https"
    } else {
        "http"
    });
    let _ = target.set_username("");
    let _ = target.set_password(None);
    target.set_query(None);
    target.set_fragment(None);
    if target.scheme() == "https" {
        target.set_path("/");
    }
    cf_url(target.as_str())
}

pub(super) fn resolve(pac: &str, target: &Url) -> Result<Settings, String> {
    let pac = cf_url(pac)?;
    let target = target_url(target)?;
    evaluate(|context| unsafe {
        CFNetworkExecuteProxyAutoConfigurationURL(
            pac.as_concrete_TypeRef(),
            target.as_concrete_TypeRef(),
            completed,
            context,
        )
    })
}

fn evaluate(start: impl FnOnce(*mut Context) -> CFRunLoopSourceRef) -> Result<Settings, String> {
    let mut result: Option<Result<Settings, String>> = None;
    let mut context = Context {
        version: 0,
        info: (&mut result as *mut Option<Result<Settings, String>>).cast(),
        retain: None,
        release: None,
        description: None,
    };
    let raw = start(&mut context);
    if raw.is_null() {
        return Err("Cannot start macOS PAC resolver".into());
    }
    let source = unsafe { CFRunLoopSource::wrap_under_create_rule(raw) };
    let runloop = CFRunLoop::get_current();
    let mode = CFString::new("VoiceXProxyResolution");
    runloop.add_source(&source, mode.as_concrete_TypeRef());
    let deadline = Instant::now() + Duration::from_secs(8);
    while result.is_none() && Instant::now() < deadline {
        CFRunLoop::run_in_mode(
            mode.as_concrete_TypeRef(),
            deadline.saturating_duration_since(Instant::now()),
            true,
        );
    }
    // Apple documents invalidation as the way to terminate a pending PAC request.
    unsafe {
        CFRunLoopSourceInvalidate(source.as_concrete_TypeRef());
    }
    runloop.remove_source(&source, mode.as_concrete_TypeRef());
    drop(source);
    result.unwrap_or_else(|| Err("macOS PAC resolution timed out / 自动代理脚本解析超时".into()))
}

unsafe extern "C" fn completed(context: *mut c_void, proxies: CFArrayRef, error: CFErrorRef) {
    let result = &mut *context.cast::<Option<Result<Settings, String>>>();
    // No exception or provider URL is exposed in this diagnostic.
    *result = Some(if !error.is_null() {
        Err("macOS PAC download/evaluation failed / 自动代理脚本下载或执行失败".into())
    } else if proxies.is_null() {
        Err("macOS PAC returned no proxy list".into())
    } else {
        parse_result(proxies)
    });
}

unsafe fn parse_result(raw: CFArrayRef) -> Result<Settings, String> {
    let proxies = CFArray::<CFType>::wrap_under_get_rule(raw);
    let first = proxies.get(0).ok_or("PAC returned an empty proxy list")?;
    let untyped = first
        .downcast::<CFDictionary>()
        .ok_or("Invalid PAC proxy entry")?;
    let proxy =
        CFDictionary::<CFString, CFType>::wrap_under_get_rule(untyped.as_concrete_TypeRef());
    let kind = proxy
        .find(kCFProxyTypeKey)
        .and_then(|v| v.downcast::<CFString>())
        .ok_or("PAC proxy type is missing")?;
    if kind == CFString::wrap_under_get_rule(kCFProxyTypeNone) {
        return Ok(Settings::default());
    }
    let transport = if kind == CFString::wrap_under_get_rule(kCFProxyTypeHTTP) {
        "http"
    }
    // CFNetwork labels a PROXY result HTTPS for an HTTPS target;
    // it still denotes an HTTP CONNECT proxy, not TLS to the proxy.
    else if kind == CFString::wrap_under_get_rule(kCFProxyTypeHTTPS) {
        "http"
    } else if kind == CFString::wrap_under_get_rule(kCFProxyTypeSOCKS) {
        "socks5h"
    } else {
        return Err("Unsupported PAC proxy type".into());
    };
    let host = proxy
        .find(kCFProxyHostNameKey)
        .and_then(|v| v.downcast::<CFString>())
        .ok_or("PAC proxy host is missing")?
        .to_string();
    let port = proxy
        .find(kCFProxyPortNumberKey)
        .and_then(|v| v.downcast::<CFNumber>())
        .and_then(|v| v.to_i64())
        .filter(|p| (1..=65535).contains(p))
        .ok_or("PAC proxy port is invalid")?;
    let host = if host.contains(':') && !host.starts_with('[') {
        format!("[{host}]")
    } else {
        host
    };
    let mut settings = Settings::default();
    settings
        .proxies
        .insert("all".into(), format!("{transport}://{host}:{port}"));
    Ok(settings)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn native_pac_evaluates_proxy_and_direct_rules() {
        let script = CFString::new("function FindProxyForURL(url, host) { return host == 'api.openai.com' ? 'PROXY proxy.example:8080' : 'DIRECT'; }");
        for (host, expected) in [
            ("api.openai.com", Some("http://proxy.example:8080")),
            ("other.example", None),
        ] {
            let target = target_url(
                &Url::parse(&format!("wss://{host}/private?key=not-sent-to-pac")).unwrap(),
            )
            .unwrap();
            let result = evaluate(|context| unsafe {
                CFNetworkExecuteProxyAutoConfigurationScript(
                    script.as_concrete_TypeRef(),
                    target.as_concrete_TypeRef(),
                    completed,
                    context,
                )
            })
            .unwrap();
            assert_eq!(result.proxies.get("all").map(String::as_str), expected);
        }
    }
}
