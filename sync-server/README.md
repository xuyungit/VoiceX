# VoiceX Sync v2 server

The relay stores encrypted envelopes in SQLite and notifies paired devices over SSE. Each device chooses its own sync categories. Upgrade the server before enabling dictionary, rule or custom prompt sync.

## First device

From this directory, build and start the relay. Use a dedicated v2 database. Existing v1 installations need the offline migration before changing databases.

```bash
cargo build --release --locked
export VOICEX_SYNC_DB="$PWD/sync-v2.db"
./target/release/voicex-sync-server serve --addr 127.0.0.1:8787
```

In another terminal, point the admin commands at the same database.

```bash
export VOICEX_SYNC_DB="$PWD/sync-v2.db"
./target/release/voicex-sync-server admin create-account --label personal
./target/release/voicex-sync-server admin create-pairing-code --account <account-id>
```

Open **Settings → Sync → First setup** and enter the server URL and pairing code. Keep the recovery key private. From a paired device, generate a single-use sync code for the next device. This code contains a reusable recovery key. Share it privately. Use HTTPS for public deployments to protect device credentials in transit.

## Operations

Configure the executable and working directory in `deploy/voicex-sync.service` and install the unit. Copy `deploy/env.example` to `/etc/voicex-sync/env` and set the address, v2 database path and log directory. Set `VOICEX_SYNC_TRUST_PROXY=true` only when the service is accessible exclusively through a trusted reverse proxy.

```bash
systemctl daemon-reload
systemctl enable --now voicex-sync
curl --fail http://127.0.0.1:8787/healthz
journalctl -u voicex-sync -f
```

Use `admin list-devices --account <account-id>` to inspect devices and `admin revoke-device --account <account-id> --device <device-id>` to revoke a device token.
