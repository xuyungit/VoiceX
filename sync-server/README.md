# VoiceX Sync Server (local dev)

This is a minimal sync server for VoiceX history synchronization (Phase 1).

## Run

```bash
cd sync-server
export VOICEX_SYNC_SHARED_SECRET="dev-shared-secret"
cargo run -- --addr 127.0.0.1:8787 --db ./dev.db
```

Environment variables (optional):

- `VOICEX_SYNC_ADDR` (same as `--addr`)
- `VOICEX_SYNC_DB` (same as `--db`)
- `VOICEX_SYNC_SHARED_SECRET` (required for auth)
- `VOICEX_SYNC_LOG_DIR` (default `./logs`)

Logs are written to `${VOICEX_SYNC_LOG_DIR}/voicex-sync.log`.

## Quick smoke test (curl)

```bash
# Generate a token once and reuse it across devices.
export TOKEN="dev-token"
export SHARED_SECRET="dev-shared-secret"

AUTH=$(python - <<'PY'
import hashlib, hmac, os
token = os.environ["TOKEN"].encode()
secret = os.environ["SHARED_SECRET"].encode()
payload = hashlib.sha256(token).hexdigest()
sig = hmac.new(secret, payload.encode(), hashlib.sha256).hexdigest()
print(f"vx1.{payload}.{sig}")
PY
)

curl -sS -H "Authorization: Bearer $AUTH" http://127.0.0.1:8787/healthz

# Register device name
curl -sS -X PUT \
  -H "Authorization: Bearer $AUTH" \
  -H "Content-Type: application/json" \
  -d '{"deviceId":"dev-device-a","deviceName":"MacBook"}' \
  http://127.0.0.1:8787/v1/device

# Post a history upsert event
curl -sS -X POST \
  -H "Authorization: Bearer $AUTH" \
  -H "Content-Type: application/json" \
  -d '{
    "deviceId":"dev-device-a",
    "events":[
      {
        "eventId":"11111111-1111-1111-1111-111111111111",
        "type":"history.upsert",
        "record":{
          "id":"22222222-2222-2222-2222-222222222222",
          "sourceDeviceId":"dev-device-a",
          "timestamp":"2026-01-24T12:00:00Z",
          "text":"hello world",
          "originalText":null,
          "aiCorrectionApplied":false,
          "mode":"push_to_talk",
          "durationMs":1200,
          "isFinal":true,
          "errorCode":0
        }
      }
    ]
  }' \
  http://127.0.0.1:8787/v1/events

# Fetch events since seq=0
curl -sS -H "Authorization: Bearer $AUTH" "http://127.0.0.1:8787/v1/events?since=0&limit=50"
```
