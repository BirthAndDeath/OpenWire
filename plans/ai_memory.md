# OpenWire Project Memory

## Project Overview
OpenWire is a P2P encrypted chat application using libp2p (Rust) with post-quantum cryptography.
- License: AGPL-3.0
- Status: v0.1.x demo, not audited
- Primary Language: Rust (Cargo workspace, 7 members)
- Frontend: Svelte 5 + SvelteKit + TypeScript (Tauri 2 webview)
- Desktop Runtime: Tauri 2

## Directory Structure

```
OpenWire/
├── openwire_core/          # Core library: P2P, crypto, storage, file transfer
│   └── src/
│       ├── actor/p2p/      # P2pActor (owns libp2p Swarm, event loop)
│       │   ├── mod.rs         # P2pCommand/P2pEvent enums, P2pActor struct, handle_command
│       │   ├── relay_handler.rs  # Relay dial/reconnect/server logic
│       │   ├── swarm_ops.rs     # Swarm operation helpers
│       │   └── netevent.rs      # FriendOnline handler (deprecated)
│       ├── core/            # ChatCore: business logic, event loop, message ops
│       │   ├── mod.rs         # ChatCore struct, try_init()
│       │   ├── event_loop.rs  # Main event loop, DHT publish, contact discovery
│       │   ├── message_ops.rs # send_text, retry_pending, dht_lookup_peerid
│       │   ├── contact_ops.rs # add_contact, discover_contact
│       │   ├── file_transfer.rs # File download/upload, chunk handling
│       │   ├── identity_ops.rs # generate/select/delete identity
│       │   ├── command_handler.rs # ChatCommand dispatcher
│       │   ├── timers.rs      # Periodic timer tasks
│       │   └── dht_ops.rs     # publish_identity_to_dht
│       ├── p2p/             # P2P types, DHT cache, protocol definitions
│       │   ├── events.rs     # handle_incoming_request, message verification/decryption
│       │   ├── netevent.rs   # NetEventRequest/Response (FriendOnline, DiscoverPeer)
│       │   ├── dht_cache.rs  # Thread-safe in-memory DHT cache
│       │   ├── swarm.rs      # Swarm initialization, Kademlia config
│       │   ├── nodes.rs      # Relay/bootstrap node config
│       │   ├── behaviour.rs  # MyBehaviour composite
│       │   └── dht.rs        # lookup_peerid_by_pubkey
│       ├── storage/         # SQLite storage layer
│       │   ├── identity.rs   # Identity CRUD
│       │   ├── contact.rs    # Contact CRUD
│       │   ├── message.rs    # Message CRUD, pending tracking
│       │   ├── sent_file.rs  # Sent file history
│       │   └── migrations.rs # Schema migrations
│       ├── message/         # Protocol message types
│       │   ├── mod.rs        # ChatMessage, ChatResponse, ChatMessageType
│       │   └── file_stream.rs # FileHashInfo, FileStreamChunk, DownloadRequest/Response
│       ├── crypto.rs        # ML-KEM + AES-GCM hybrid encryption
│       ├── signature.rs     # ML-DSA key generation, signing, verification
│       ├── identity.rs      # CompleteIdentity, generate/load identity
│       ├── command.rs       # ChatCommand, MessageEvent, IncomingMessage enums
│       ├── compression.rs   # zstd compression
│       ├── transfer.rs      # FileTransferState, compute_file_hash
│       ├── coreconfig.rs    # CoreConfig struct
│       ├── corehandle.rs    # CoreHandle for external control
│       ├── log.rs           # Logger initialization
│       ├── error.rs         # Error types (CryptoError, P2pError, etc.)
│       └── diagnostics.rs   # DHT/crypto self-test
├── openwire/               # Tauri desktop app
│   └── src-tauri/src/
│       └── lib.rs           # 21 Tauri commands, AppData, run(), event loop
├── rootcell/               # Secure key storage (platform keyring)
│   └── src/identity.rs      # PrivateKeyHandle: AES-GCM encrypted files + keyring
├── openwire_cli/           # CLI app (ratatui TUI)
├── openwire_server/        # Relay server
│   ├── common/src/lib.rs   # Relay server implementation
│   └── server_cli/         # Server CLI binary
└── libp2p-pathranker/      # libp2p path ranking (optional)
```

## P2P Architecture

### Transport Stack
- TCP + Noise + Yamux (primary)
- QUIC (experimental)
- WebSocket (browser support)
- Circuit Relay v2 (NAT traversal)
- DCUtR (direct connection upgrade after relay)

### Network Behaviours (MyBehaviour)
- `kademlia` — DHT (`/chat/kad/0.0.1`)
- `relay_client` / `relay_server` — circuit relay v2
- `identify` — address/protocol exchange
- `dcutr` — direct connection upgrade
- `mdns` — LAN discovery
- `rr_msg` / `rr_netevent` — custom request-response protocols
- `autonat` — NAT status detection
- `ping`, `connection_limits`

### Message Flow
```
Sender (by ML-DSA pubkey hex)
  → dht_lookup_peerid() [memory cache → connected peers → DHT cache → network GetProviders]
  → lookup_mlkem_pubkey() [DHT cache → contacts DB → DHT GetRecord]
  → encrypt_message(data, recipient_mlkem_pubkey) [ML-KEM encapsulate + AES-GCM]
  → build_signed_message() [ML-DSA signature + timestamp + nonce]
  → send_message(peer_id, signed_message) [libp2p request-response]
  → Recipient: verify signature → decrypt → store in SQLite → delivery receipt
```

### ChatCore Event Loop
```
run_inner():
  1. Start DHT registration timer (5 min)
  2. Start retry pending timer (30s)
  3. Start routing table save timer (5 min)
  4. Start DHT discovery timer (30s)
  5. Start DHT cleanup timer (1 hour)
  6. Publish identity to DHT
  7. tokio::select! → cmd_rx + p2p_event_rx + timers
```

### P2pActor (owns Swarm)
```
start_p2p_actor():
  tokio::select! {
    swarm.select_next_some() → handle_swarm_event()
    rx.recv() → handle_command() (or Shutdown)
    cancellation_token.cancelled() → save routing table, exit
  }
```

## Security Architecture

### Post-Quantum Cryptography
- ML-DSA-65 (FIPS 204) — identity signatures (1952-byte public key, 4032-byte private key, 3309-byte signature)
- ML-KEM-768 (FIPS 203) — key encapsulation for session encryption
- AES-256-GCM — symmetric encryption (after ML-KEM encapsulation)

### Identity Model
- Each identity = ML-DSA keypair (persistent) + ML-KEM keypair (session-ephemeral, regenerated per startup)
- PeerID (Ed25519) — transport-level, regenerated per session
- Private keys stored via rootcell (platform keyring + AES-GCM encrypted file)

### Key Storage (rootcell)
- Master key (32 bytes) stored in platform keyring (Windows Credential Manager, macOS Keychain, etc.)
- Per-identity private keys encrypted with AES-256-GCM, stored in `keys/` directory
- HKDF key derivation per identity (each identity gets unique AES key from master key)
- Memory locking via `mlock`/`VirtualLock`
- Zeroizing on drop

### DHT Identity
- `start_providing(pubkey_hex)` registers PeerID as provider
- `put_record("mlkem:{pubkey_hex}")` stores ML-KEM public key
- `get_providers(pubkey_hex)` discovers PeerIDs for a pubkey
- `get_record("mlkem:{pubkey_hex}")` retrieves ML-KEM public key

## NetEvent Protocol

### Request-Response Protocol (`rr_netevent`)
- Used for network events separate from message delivery
- `#[non_exhaustive]` enums for extensibility

### NetEventRequest
- `FriendOnline { mldsa_pubkey_hex, peer_id, listen_addrs, mlkem_pubkey_hex }` — sent on connection
- `DiscoverPeer { mldsa_pubkey_hex }` — relay-based peer discovery

### NetEventResponse
- `Ack` — simple acknowledgment
- `PeerInfo { mldsa_pubkey_hex, peer_id, mlkem_pubkey_hex }` — relay discovery result

## Discovery Mechanisms

### 1. DHT Kademlia (primary)
- `start_providing(pubkey)` → provider records
- `get_providers(pubkey)` → discover peer PeerIDs
- Bootstrap against configured bootstrap nodes

### 2. FriendOnline (on connection)
- Sent immediately on `ConnectionEstablished`
- Carries full identity (pubkey, PeerID, ML-KEM key, listen addrs)
- Recipient caches identity and dials listen addrs

### 3. DiscoverPeer (relay-based, fallback)
- Sent to relay nodes when DHT fails
- Relay checks local DHT cache and returns PeerInfo
- Client constructs circuit address and dials

### 4. mDNS (LAN)
- Local network discovery via mDNS

## Tauri Commands (21 total)

### Messaging
- `send` — Send text message
- `send_file` — Send file (compute hash, record, send FileHashInfo)
- `load_messages` — Load paginated message history
- `delete_message` — Delete a message (with ownership check)
- `list_contacts` — List all contacts
- `add_contact` — Add a new contact
- `discover_contact` — Discover contact via DHT
- `delete_contact` — Delete contact (with confirmation dialog)

### Identity
- `list_identities` — List all identities
- `select_identity` — Switch current identity
- `delete_identity` — Delete an identity
- `generate_identity` — Generate new identity
- `get_identity_qr_data` — Get QR data for current identity
- `is_keyring_available` — Check system keyring

### File Transfer
- `request_file_download` — Request file download from peer
- `list_sent_files` — List all sent files
- `delete_sent_file` — Delete sent file record

### Configuration
- `get_nodes_config` — Get relay/bootstrap node config
- `save_nodes_config` — Save node config (with multiaddr validation)
- `reset_nodes_config` — Reset node config to defaults

### System
- `check_core_ready` — Check if core is initialized

## ChatCore State

### Fields (19)
- `p2p_handle: P2pActorHandle` — P2pActor communication
- `rx_p2p_event: mpsc::Receiver<P2pEvent>` — Network event receiver
- `identity_keypair: Keypair` — Transport Ed25519 keypair
- `tx_message: mpsc::Sender<ChatcoreEvent>` — UI event sender
- `rx_cmd: mpsc::Receiver<ChatCommand>` — Command receiver
- `data_dir: PathBuf` — Data directory
- `core_handle: CoreHandle` — External control handle
- `mldsa_pubkey_hex: Option<String>` — ML-DSA public key hex
- `current_peer_id: Option<PeerId>` — Current PeerID
- `mldsa_identity_id: Option<String>` — ML-DSA identity ID
- `mlkem_pubkey_hex: Option<String>` — ML-KEM public key hex
- `mldsa_private_key: Option<Zeroizing<Vec<u8>>>` — Cached ML-DSA private key
- `file_transfers: HashMap<String, FileTransferState>` — Active file transfers
- `dht_cache: Arc<DhtCache>` — DHT cache
- `connected_peers: HashMap<PeerId, usize>` — Connected peers with counts
- `peerid_to_pubkey: HashMap<PeerId, String>` — PeerID → pubkey mapping
- `mlkem_decap_key: Option<DecapsulationKey>` — ML-KEM decapsulation key
- `relay_nodes: Vec<(String, String)>` — Configured relay nodes
- `bootstrap_ready_received: bool` — Bootstrap ready flag
- `seen_hashes: LruCache<Vec<u8>, ()>` — Deduplication for replay protection

## Known Issues

### Security (Fixed)
- S1: `send_file` path traversal → canonicalize + sandbox check
- S2: `validate_contact` fail-open → mandatory DB check
- S3: `delete_message` no auth → ownership check
- S4: `save_nodes_config` no validation → multiaddr format check
- S5: log.rs expect() panic → error propagation
- S6: empty pubkey hex accepted → length check
- S7: DHT identity binding missing → peer-to-pubkey verification
- S8: DHT ML-KEM unauthenticated → set publisher
- S9: 1-hour replay window → seen_hashes LRU cache
- S10: Master key no KDF → HKDF derivation
- S11: Master key not zeroized → Zeroizing wrapper
- S12: data_dir path traversal → canonicalization
- S13: delete_master_key on error → error propagation

### Architecture (Simplified)
- Actor trait removed (was over-engineered, only DownloadResponseActor used it)
- download_actor.rs removed (feature-gated, unused)
- P2pActorHandle simplified (direct P2pCommand channel, no ActorCommand wrapper)

### Functional (To Fix)
- F1: `send_response` drops ResponseChannel when private key not cached
- F2: File share format mismatch between DB and frontend parser
- F3: `select_identity`/`reinitialize_swarm` passes empty bootstrap_nodes
- F4: `delete_contact` dialog await has no timeout
- F5: `delete_contact` proceeds after message deletion failure
- F6: Identity deletion doesn't clear peerid_to_pubkey/connected_peers
- F7: `mark_sent_by_hash` error silently ignored
- F8: DHT publish failure silently dropped
- F9: Contact lastMsg updated with wrong key
- F10: handleScroll parameter type mismatch
- F11: CLI status bar covers last messages
- F12: CLI detect_file_share uses rfind instead of find

### Planned: Multi-Device Support (Interim)
- Extend DHT cache from 1:1 to 1:N (pubkey → multiple PeerIDs)
- Add devices table + message_sync_status table
- Extend NetEvent with SyncMessage/DeviceAnnounce variants
- Sender delivers to ONE random device
- Receiving device syncs to its other devices via NetEvent

## Future Architecture: Post-Quantum libp2p Standardization

### When libp2p adds native QPC support (Noise_XK + QPC handshake)

### Current Architecture (Two-Lookup)
```
DHT: ML-DSA pubkey → PeerID          (1:1, overwrites on update)
DHT: PeerID → ML-KEM pubkey           (separate lookup, unauthenticated record)
Sender: Look up pubkey → PeerID → ML-KEM key → encrypt → send
Problem: Two DHT lookups, ML-KEM is session-ephemeral, sender manages all devices
```

### Future Architecture (Standard libp2p)
```
DHT: PeerID → Multiaddr[ ]            (1:N, standard Kademlia)
Transport: Noise_XK + QPC hybrid handshake
  → ML-KEM-768 encapsulated in Noise handshake payload
  → ML-DSA-65 signs handshake for identity binding
  → Session key derived from ML-KEM secret + Noise ephemeral
Sender: Query DHT for PeerID multiaddrs → pick random → connect → handshake → send
```

### Key Simplifications
| Current | Future |
|---------|--------|
| `DhtCache::pubkey_peerid` (1:1 map) | Kademlia provider records (1:N) |
| `DhtCache::mlkem_pubkeys` (separate map) | Removed entirely |
| Two DHT lookups per message | One lookup (PeerID → multiaddrs) |
| Sender encrypts with recipient's ML-KEM key | Inline QPC handshake establishes session key |
| Sender manages ML-KEM key freshness | Transport handles key agreement |
| Sender must deliver to all devices | Sender delivers to ONE device, device syncs rest |

### Multi-Device Sync (Unchanged)
- Device A receives message via QPC handshake
- Device A queries DHT for its own identity's other devices
- Device A connects to Device B/C via QPC handshake
- Device A sends `NetEventRequest::SyncMessage` (ML-DSA signed)
- Device B/C verify signature, decrypt, store, Ack

### Backward Compatibility
- Old clients: pubkey → peerid → mlkem (two-step lookup)
- New clients: QPC handshake first, fall back to two-step
- Dual-publish DHT records during transition
- All messages ML-DSA signed regardless of transport layer