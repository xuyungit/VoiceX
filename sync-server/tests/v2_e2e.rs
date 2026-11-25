use futures_util::StreamExt;
use reqwest::{Client, StatusCode};
use std::time::Duration;
use tempfile::TempDir;
use tokio::net::TcpListener;
use voicex_sync_protocol::{
    error_codes, AccountInfoResponse, ApiErrorBody, DeviceUpdateRequest, EnvelopeStatus,
    EnvelopeSubmission, NotifyEvent, PairRedeemRequest, PairRedeemResponse, PullEnvelopesResponse,
    PushEnvelopesRequest, PushEnvelopesResponse, SyncDomain, MAX_PUSH_BODY_BYTES,
};
use voicex_sync_server::api::{build_app, AppState};
use voicex_sync_server::db;

struct TestServer {
    base_url: String,
    pool: sqlx::SqlitePool,
    _dir: TempDir,
}

async fn spawn_server() -> TestServer {
    spawn_server_with_limits(120, Duration::from_secs(60)).await
}

async fn spawn_server_with_limits(max_req: usize, window: Duration) -> TestServer {
    let dir = TempDir::new().unwrap();
    let db_path = dir.path().join("test-v2.db");
    let pool = db::open_pool(db_path.to_str().unwrap()).await.unwrap();
    let server_id = db::ensure_server_id(&pool).await.unwrap();
    let state = AppState::with_rate_limit(pool.clone(), server_id, max_req, window);
    let app = build_app(state);

    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });

    TestServer {
        base_url: format!("http://{addr}"),
        pool,
        _dir: dir,
    }
}

async fn pair_device(
    srv: &TestServer,
    client: &Client,
    account_id: &str,
    device_id: &str,
    device_name: &str,
    platform: &str,
) -> PairRedeemResponse {
    let code = db::create_pairing_code(&srv.pool, account_id, 15)
        .await
        .unwrap();
    let resp = client
        .post(format!("{}/v2/pair/redeem", srv.base_url))
        .json(&PairRedeemRequest {
            pairing_code: code.pairing_code,
            device_id: device_id.into(),
            device_name: device_name.into(),
            platform: platform.into(),
            app_version: "0.20.0".into(),
        })
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    resp.json::<PairRedeemResponse>().await.unwrap()
}

#[tokio::test]
async fn pairing_is_single_use_and_revocation_only_blocks_revoked_device() {
    let srv = spawn_server().await;
    let client = Client::new();
    let acc = db::create_account(&srv.pool, "personal", db::DEFAULT_STORAGE_QUOTA_BYTES)
        .await
        .unwrap();

    // 1. Single-use pairing code succeeds once, second redeem fails with 409
    let code = db::create_pairing_code(&srv.pool, &acc.account_id, 15)
        .await
        .unwrap();
    let req = PairRedeemRequest {
        pairing_code: code.pairing_code.to_lowercase().replace('-', " "),
        device_id: "mac-1".into(),
        device_name: "MBP".into(),
        platform: "macos".into(),
        app_version: "0.20.0".into(),
    };
    let r1 = client
        .post(format!("{}/v2/pair/redeem", srv.base_url))
        .json(&req)
        .send()
        .await
        .unwrap();
    assert_eq!(r1.status(), StatusCode::OK);
    let mac = r1.json::<PairRedeemResponse>().await.unwrap();

    let r2 = client
        .post(format!("{}/v2/pair/redeem", srv.base_url))
        .json(&req)
        .send()
        .await
        .unwrap();
    assert_eq!(r2.status(), StatusCode::CONFLICT);
    let err: ApiErrorBody = r2.json().await.unwrap();
    assert_eq!(err.code, error_codes::PAIRING_INVALID_OR_EXPIRED);

    // 2. Expired pairing code fails with 409
    let expired_code = db::create_pairing_code(&srv.pool, &acc.account_id, -1)
        .await
        .unwrap();
    let r_exp = client
        .post(format!("{}/v2/pair/redeem", srv.base_url))
        .json(&PairRedeemRequest {
            pairing_code: expired_code.pairing_code,
            device_id: "win-exp".into(),
            device_name: "Win".into(),
            platform: "windows".into(),
            app_version: "0.20.0".into(),
        })
        .send()
        .await
        .unwrap();
    assert_eq!(r_exp.status(), StatusCode::CONFLICT);

    // 3. Pair second device, revoke first -> first gets 401 device_revoked, second keeps working
    let win = pair_device(&srv, &client, &acc.account_id, "win-1", "Yoga", "windows").await;
    assert!(db::revoke_device(&srv.pool, &acc.account_id, "mac-1")
        .await
        .unwrap());

    let mac_acc = client
        .get(format!("{}/v2/account", srv.base_url))
        .bearer_auth(&mac.device_token)
        .send()
        .await
        .unwrap();
    assert_eq!(mac_acc.status(), StatusCode::UNAUTHORIZED);
    let mac_err: ApiErrorBody = mac_acc.json().await.unwrap();
    assert_eq!(mac_err.code, error_codes::DEVICE_REVOKED);

    let win_acc = client
        .get(format!("{}/v2/account", srv.base_url))
        .bearer_auth(&win.device_token)
        .send()
        .await
        .unwrap();
    assert_eq!(win_acc.status(), StatusCode::OK);
    let info: AccountInfoResponse = win_acc.json().await.unwrap();
    assert_eq!(info.devices.len(), 2);
}

#[tokio::test]
async fn stats_revision_101_then_100_keeps_101_and_new_device_only_sees_latest() {
    let srv = spawn_server().await;
    let client = Client::new();
    let acc = db::create_account(&srv.pool, "personal", db::DEFAULT_STORAGE_QUOTA_BYTES)
        .await
        .unwrap();
    let mac = pair_device(&srv, &client, &acc.account_id, "mac-1", "MBP", "macos").await;
    let win = pair_device(&srv, &client, &acc.account_id, "win-1", "Yoga", "windows").await;

    // Push revision 101 first, then out-of-order revision 100
    let push_101 = client
        .post(format!("{}/v2/envelopes", srv.base_url))
        .bearer_auth(&mac.device_token)
        .json(&PushEnvelopesRequest {
            envelopes: vec![EnvelopeSubmission {
                op_id: "stats:mac-1:totals:101".into(),
                domain: SyncDomain::Stats,
                object_id: "mac-1:totals".into(),
                revision: 101,
                nonce: "bm9uY2UxMDE=".into(),
                ciphertext: "Y2lwaGVyMTAx".into(),
            }],
        })
        .send()
        .await
        .unwrap()
        .json::<PushEnvelopesResponse>()
        .await
        .unwrap();
    assert_eq!(push_101.results[0].status, EnvelopeStatus::Accepted);

    let push_100 = client
        .post(format!("{}/v2/envelopes", srv.base_url))
        .bearer_auth(&mac.device_token)
        .json(&PushEnvelopesRequest {
            envelopes: vec![EnvelopeSubmission {
                op_id: "stats:mac-1:totals:100".into(),
                domain: SyncDomain::Stats,
                object_id: "mac-1:totals".into(),
                revision: 100,
                nonce: "bm9uY2UxMDA=".into(),
                ciphertext: "Y2lwaGVyMTAw".into(),
            }],
        })
        .send()
        .await
        .unwrap()
        .json::<PushEnvelopesResponse>()
        .await
        .unwrap();
    assert_eq!(push_100.results[0].status, EnvelopeStatus::StaleIgnored);

    // Retry of 101 with same op_id returns Duplicate with original seq
    let retry_101 = client
        .post(format!("{}/v2/envelopes", srv.base_url))
        .bearer_auth(&mac.device_token)
        .json(&PushEnvelopesRequest {
            envelopes: vec![EnvelopeSubmission {
                op_id: "stats:mac-1:totals:101".into(),
                domain: SyncDomain::Stats,
                object_id: "mac-1:totals".into(),
                revision: 101,
                nonce: "bm9uY2UxMDE=".into(),
                ciphertext: "Y2lwaGVyMTAx".into(),
            }],
        })
        .send()
        .await
        .unwrap()
        .json::<PushEnvelopesResponse>()
        .await
        .unwrap();
    assert_eq!(retry_101.results[0].status, EnvelopeStatus::Duplicate);
    assert_eq!(retry_101.results[0].seq, push_101.results[0].seq);

    // Push 102 -> supersedes 101 so fresh device only sees 102
    let push_102 = client
        .post(format!("{}/v2/envelopes", srv.base_url))
        .bearer_auth(&mac.device_token)
        .json(&PushEnvelopesRequest {
            envelopes: vec![EnvelopeSubmission {
                op_id: "stats:mac-1:totals:102".into(),
                domain: SyncDomain::Stats,
                object_id: "mac-1:totals".into(),
                revision: 102,
                nonce: "bm9uY2UxMDI=".into(),
                ciphertext: "Y2lwaGVyMTAy".into(),
            }],
        })
        .send()
        .await
        .unwrap()
        .json::<PushEnvelopesResponse>()
        .await
        .unwrap();
    assert_eq!(push_102.results[0].status, EnvelopeStatus::Accepted);

    let pulled = client
        .get(format!("{}/v2/envelopes?domain=stats&since=0&limit=50", srv.base_url))
        .bearer_auth(&win.device_token)
        .send()
        .await
        .unwrap()
        .json::<PullEnvelopesResponse>()
        .await
        .unwrap();
    assert_eq!(pulled.items.len(), 1);
    assert_eq!(pulled.items[0].revision, 102);
    assert!(!pulled.has_more);
}

#[tokio::test]
async fn scope_enforcement_pagination_and_sse_notify_work_end_to_end() {
    let srv = spawn_server().await;
    let client = Client::new();
    let acc = db::create_account(&srv.pool, "personal", db::DEFAULT_STORAGE_QUOTA_BYTES)
        .await
        .unwrap();
    let mac = pair_device(&srv, &client, &acc.account_id, "mac-1", "MBP", "macos").await;
    let win = pair_device(&srv, &client, &acc.account_id, "win-1", "Yoga", "windows").await;

    // Mac sets keyCheckTag and narrows Windows to stats-only (on Windows' own token)
    let upd_mac = client
        .put(format!("{}/v2/devices/mac-1", srv.base_url))
        .bearer_auth(&mac.device_token)
        .json(&DeviceUpdateRequest {
            device_name: "MBP_M5".into(),
            platform: "macos".into(),
            app_version: "0.20.0".into(),
            allow_stats: true,
            allow_history: true,
            allow_edits: true,
            key_check_tag: Some("deadbeef00112233".into()),
        })
        .send()
        .await
        .unwrap();
    assert_eq!(upd_mac.status(), StatusCode::OK);

    // Cross-device update is forbidden
    let cross_upd = client
        .put(format!("{}/v2/devices/mac-1", srv.base_url))
        .bearer_auth(&win.device_token)
        .json(&DeviceUpdateRequest {
            device_name: "Hijack".into(),
            platform: "windows".into(),
            app_version: "0.20.0".into(),
            allow_stats: true,
            allow_history: false,
            allow_edits: false,
            key_check_tag: None,
        })
        .send()
        .await
        .unwrap();
    assert_eq!(cross_upd.status(), StatusCode::FORBIDDEN);

    // Windows sets itself stats-only
    let upd_win = client
        .put(format!("{}/v2/devices/win-1", srv.base_url))
        .bearer_auth(&win.device_token)
        .json(&DeviceUpdateRequest {
            device_name: "Yoga".into(),
            platform: "windows".into(),
            app_version: "0.20.0".into(),
            allow_stats: true,
            allow_history: false,
            allow_edits: false,
            key_check_tag: Some("deadbeef00112233".into()),
        })
        .send()
        .await
        .unwrap();
    assert_eq!(upd_win.status(), StatusCode::OK);

    // Stats-only Windows cannot push history.create or spoof Mac's stats shard
    let win_push_hist = client
        .post(format!("{}/v2/envelopes", srv.base_url))
        .bearer_auth(&win.device_token)
        .json(&PushEnvelopesRequest {
            envelopes: vec![EnvelopeSubmission {
                op_id: "create:r1".into(),
                domain: SyncDomain::History,
                object_id: "r1".into(),
                revision: 0,
                nonce: "bm9uY2U=".into(),
                ciphertext: "Y2lwaGVy".into(),
            }],
        })
        .send()
        .await
        .unwrap();
    assert_eq!(win_push_hist.status(), StatusCode::FORBIDDEN);

    let win_spoof_stats = client
        .post(format!("{}/v2/envelopes", srv.base_url))
        .bearer_auth(&win.device_token)
        .json(&PushEnvelopesRequest {
            envelopes: vec![EnvelopeSubmission {
                op_id: "stats:mac-1:totals:1".into(),
                domain: SyncDomain::Stats,
                object_id: "mac-1:totals".into(),
                revision: 1,
                nonce: "bm9uY2U=".into(),
                ciphertext: "Y2lwaGVy".into(),
            }],
        })
        .send()
        .await
        .unwrap();
    assert_eq!(win_spoof_stats.status(), StatusCode::FORBIDDEN);

    // Subscribe to SSE on Windows BEFORE Mac pushes 3 history items
    let sse_resp = client
        .get(format!("{}/v2/subscribe", srv.base_url))
        .bearer_auth(&win.device_token)
        .send()
        .await
        .unwrap();
    assert_eq!(sse_resp.status(), StatusCode::OK);
    assert_eq!(
        sse_resp
            .headers()
            .get("x-accel-buffering")
            .and_then(|v| v.to_str().ok()),
        Some("no")
    );
    let mut sse_stream = sse_resp.bytes_stream();

    let push_mac = client
        .post(format!("{}/v2/envelopes", srv.base_url))
        .bearer_auth(&mac.device_token)
        .json(&PushEnvelopesRequest {
            envelopes: (1..=3)
                .map(|i| EnvelopeSubmission {
                    op_id: format!("create:r{i}"),
                    domain: SyncDomain::History,
                    object_id: format!("r{i}"),
                    revision: 0,
                    nonce: "bm9uY2U=".into(),
                    ciphertext: format!("Y2lwaGVy{i}"),
                })
                .collect(),
        })
        .send()
        .await
        .unwrap();
    assert_eq!(push_mac.status(), StatusCode::OK);

    // Receive at least one SSE notify event
    let chunk = tokio::time::timeout(Duration::from_secs(3), sse_stream.next())
        .await
        .expect("SSE notify timeout")
        .expect("SSE stream item")
        .expect("SSE bytes");
    let text = String::from_utf8_lossy(&chunk);
    assert!(text.contains("event: notify"), "got SSE chunk: {text}");
    let data_line = text
        .lines()
        .find_map(|l| l.strip_prefix("data:"))
        .expect("data line")
        .trim();
    let note: NotifyEvent = serde_json::from_str(data_line).unwrap();
    assert_eq!(note.domain, SyncDomain::History);
    assert!(note.seq >= 1);

    // Paginate history with limit=2 -> page 1 has 2 items + hasMore=true, page 2 has 1 item + hasMore=false
    let p1 = client
        .get(format!("{}/v2/envelopes?domain=history&since=0&limit=2", srv.base_url))
        .bearer_auth(&win.device_token)
        .send()
        .await
        .unwrap()
        .json::<PullEnvelopesResponse>()
        .await
        .unwrap();
    assert_eq!(p1.items.len(), 2);
    assert!(p1.has_more);

    let p2 = client
        .get(format!(
            "{}/v2/envelopes?domain=history&since={}&limit=2",
            srv.base_url, p1.next_cursor
        ))
        .bearer_auth(&win.device_token)
        .send()
        .await
        .unwrap()
        .json::<PullEnvelopesResponse>()
        .await
        .unwrap();
    assert_eq!(p2.items.len(), 1);
    assert!(!p2.has_more);
}

#[tokio::test]
async fn payload_size_limit_and_account_quota_are_enforced() {
    let srv = spawn_server().await;
    let client = Client::new();
    // Tiny 2 KiB quota
    let acc = db::create_account(&srv.pool, "tiny", 2048).await.unwrap();
    let mac = pair_device(&srv, &client, &acc.account_id, "mac-1", "MBP", "macos").await;

    // 1. Oversized HTTP body (> 2 MiB) -> 413 payload_too_large
    let huge = "A".repeat(MAX_PUSH_BODY_BYTES + 1024);
    let r_big = client
        .post(format!("{}/v2/envelopes", srv.base_url))
        .bearer_auth(&mac.device_token)
        .body(huge)
        .header("content-type", "application/json")
        .send()
        .await
        .unwrap();
    assert_eq!(r_big.status(), StatusCode::PAYLOAD_TOO_LARGE);
    let err_big: ApiErrorBody = r_big.json().await.unwrap();
    assert_eq!(err_big.code, error_codes::PAYLOAD_TOO_LARGE);

    // 2. Exceeding 2 KiB account quota -> 507 quota_exceeded
    let r_quota = client
        .post(format!("{}/v2/envelopes", srv.base_url))
        .bearer_auth(&mac.device_token)
        .json(&PushEnvelopesRequest {
            envelopes: vec![EnvelopeSubmission {
                op_id: "create:big".into(),
                domain: SyncDomain::History,
                object_id: "big".into(),
                revision: 0,
                nonce: "bm9uY2U=".into(),
                ciphertext: "B".repeat(3000),
            }],
        })
        .send()
        .await
        .unwrap();
    assert_eq!(r_quota.status(), StatusCode::INSUFFICIENT_STORAGE);
    let err_quota: ApiErrorBody = r_quota.json().await.unwrap();
    assert_eq!(err_quota.code, error_codes::QUOTA_EXCEEDED);
}

#[tokio::test]
async fn rate_limiting_returns_429_json_error() {
    let srv = spawn_server_with_limits(2, Duration::from_secs(60)).await;
    let client = Client::new();
    let acc = db::create_account(&srv.pool, "rl", db::DEFAULT_STORAGE_QUOTA_BYTES)
        .await
        .unwrap();
    let mac = pair_device(&srv, &client, &acc.account_id, "mac-1", "MBP", "macos").await;

    for _ in 0..2 {
        let r = client
            .get(format!("{}/v2/account", srv.base_url))
            .bearer_auth(&mac.device_token)
            .send()
            .await
            .unwrap();
        assert_eq!(r.status(), StatusCode::OK);
    }

    let r3 = client
        .get(format!("{}/v2/account", srv.base_url))
        .bearer_auth(&mac.device_token)
        .send()
        .await
        .unwrap();
    assert_eq!(r3.status(), StatusCode::TOO_MANY_REQUESTS);
    let err: ApiErrorBody = r3.json().await.unwrap();
    assert_eq!(err.code, error_codes::RATE_LIMITED);
}

#[tokio::test]
async fn authed_device_can_issue_one_time_pairing_code_for_peer() {
    let srv = spawn_server().await;
    let client = Client::new();
    let acc = db::create_account(&srv.pool, "invite", db::DEFAULT_STORAGE_QUOTA_BYTES)
        .await
        .unwrap();
    let mac = pair_device(&srv, &client, &acc.account_id, "mac-1", "MBP", "macos").await;

    // Authenticated device requests a one-time pairing code for a new peer
    let issue_resp = client
        .post(format!("{}/v2/devices/pairing-code", srv.base_url))
        .bearer_auth(&mac.device_token)
        .json(&voicex_sync_protocol::CreatePairingCodeRequest {
            ttl_minutes: Some(15),
        })
        .send()
        .await
        .unwrap();
    assert_eq!(issue_resp.status(), StatusCode::OK);
    let issued: voicex_sync_protocol::CreatePairingCodeResponse = issue_resp.json().await.unwrap();
    assert!(issued.pairing_code.starts_with("VXP2-"));

    // Peer redeems that code and joins the same account
    let redeem_resp = client
        .post(format!("{}/v2/pair/redeem", srv.base_url))
        .json(&PairRedeemRequest {
            pairing_code: issued.pairing_code.clone(),
            device_id: "win-2".into(),
            device_name: "Windows PC".into(),
            platform: "windows".into(),
            app_version: "0.20.0".into(),
        })
        .send()
        .await
        .unwrap();
    assert_eq!(redeem_resp.status(), StatusCode::OK);
    let peer: PairRedeemResponse = redeem_resp.json().await.unwrap();
    assert_eq!(peer.account_id, acc.account_id);
    assert_eq!(peer.device_id, "win-2");

    // Reusing the same code fails with 409
    let reuse_resp = client
        .post(format!("{}/v2/pair/redeem", srv.base_url))
        .json(&PairRedeemRequest {
            pairing_code: issued.pairing_code,
            device_id: "win-3".into(),
            device_name: "Other".into(),
            platform: "windows".into(),
            app_version: "0.20.0".into(),
        })
        .send()
        .await
        .unwrap();
    assert_eq!(reuse_resp.status(), StatusCode::CONFLICT);

    // Revoked device cannot issue pairing codes
    db::revoke_device(&srv.pool, &acc.account_id, "mac-1")
        .await
        .unwrap();
    let revoked_resp = client
        .post(format!("{}/v2/devices/pairing-code", srv.base_url))
        .bearer_auth(&mac.device_token)
        .json(&voicex_sync_protocol::CreatePairingCodeRequest::default())
        .send()
        .await
        .unwrap();
    assert_eq!(revoked_resp.status(), StatusCode::UNAUTHORIZED);
}
