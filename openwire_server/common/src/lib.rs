use std::num::NonZero;
use std::path::Path;
use std::str::FromStr;
use std::time::Duration;

use libp2p::futures::StreamExt;
use libp2p::kad::{self, store::MemoryStore, Config as KadConfig};
use libp2p::swarm::{NetworkBehaviour, SwarmEvent};
use libp2p::{identify, identity, noise, ping, relay, tcp, yamux, PeerId, StreamProtocol, SwarmBuilder};

const DHT_RELAY_INDEX_KEY: &str = "relay_nodes_public";
const PROTOCOL_KAD: StreamProtocol = StreamProtocol::new("/chat/kad/0.0.1");

#[derive(NetworkBehaviour)]
struct RelayBehaviour {
    relay: relay::Behaviour,
    kademlia: kad::Behaviour<MemoryStore>,
    identify: identify::Behaviour,
    ping: ping::Behaviour,
}

fn load_keypair(path: &Path) -> anyhow::Result<identity::Keypair> {
    if path.exists() {
        return Ok(identity::Keypair::from_protobuf_encoding(&std::fs::read(path)?)?);
    }
    let kp = identity::Keypair::generate_ed25519();
    std::fs::create_dir_all(path.parent().unwrap())?;
    std::fs::write(path, kp.to_protobuf_encoding()?)?;
    Ok(kp)
}

pub async fn relay(dir: Option<&Path>, port: u16) -> anyhow::Result<()> {
    let dir = dir.unwrap_or_else(|| Path::new(".openwire-relay"));
    std::fs::create_dir_all(dir)?;

    let kp = load_keypair(&dir.join("ed25519.bin"))?;
    let peer_id = kp.public().to_peer_id();
    println!("中继公钥: PeerId={peer_id}");

    let nodes_cfg = openwire_core::p2p::nodes::NodesConfig::load(dir);
    let bootstrap_nodes = nodes_cfg.bootstrap_nodes;

    let mut kad_config = KadConfig::new(PROTOCOL_KAD);
    kad_config
        .set_query_timeout(Duration::from_secs(60))
        .set_replication_factor(NonZero::new(20).unwrap())
        .set_parallelism(NonZero::new(3).unwrap())
        .set_periodic_bootstrap_interval(Some(Duration::from_secs(300)))
        .set_provider_record_ttl(Some(Duration::from_secs(3600)))
        .set_publication_interval(Some(Duration::from_secs(3600)));

    let mut swarm = SwarmBuilder::with_existing_identity(kp)
        .with_tokio()
        .with_tcp(
            tcp::Config::default(),
            noise::Config::new,
            yamux::Config::default,
        )
        .map_err(|e| anyhow::anyhow!("tcp: {e}"))?
        .with_quic()
        .with_dns()
        .map_err(|e| anyhow::anyhow!("dns: {e}"))?
        .with_relay_client(noise::Config::new, yamux::Config::default)
        .map_err(|e| anyhow::anyhow!("relay client: {e}"))?
        .with_behaviour(|key, _relay_client| {
            let relay_cfg = relay::Config {
                max_circuits: 50,
                max_circuits_per_peer: 5,
                max_reservations: 50,
                max_reservations_per_peer: 5,
                reservation_duration: Duration::from_secs(7200),
                max_circuit_duration: Duration::from_secs(3600),
                max_circuit_bytes: 100 << 20,
                ..Default::default()
            };
            let relay = relay::Behaviour::new(key.public().to_peer_id(), relay_cfg);

            let pid = key.public().to_peer_id();
            let mut kademlia = kad::Behaviour::with_config(pid, MemoryStore::new(pid), kad_config.clone());
            for node in &bootstrap_nodes {
                if let (Ok(pid), Ok(addr)) = (PeerId::from_str(&node[0]), node[1].parse()) {
                    kademlia.add_address(&pid, addr);
                }
            }
            let _ = kademlia.bootstrap();

            let identify = identify::Behaviour::new(
                identify::Config::new("/rootcell/identify/1.0.0".to_string(), key.public())
                    .with_agent_version("openwire-relay/0.1.0".to_string()),
            );
            let ping = ping::Behaviour::new(ping::Config::new());

            Ok(RelayBehaviour { relay, kademlia, identify, ping })
        })
        .map_err(|e| anyhow::anyhow!("behaviour: {e}"))?
        .build();

    swarm.listen_on(format!("/ip4/0.0.0.0/tcp/{port}").parse()?)?;
    swarm.listen_on(format!("/ip4/0.0.0.0/udp/{port}/quic-v1").parse()?)?;
    swarm.listen_on("/p2p-circuit".parse()?).ok();

    let relay_key = libp2p::kad::RecordKey::new(&DHT_RELAY_INDEX_KEY.as_bytes().to_vec());
    swarm.behaviour_mut().kademlia.start_providing(relay_key)?;

    loop {
        match swarm.select_next_some().await {
            SwarmEvent::NewListenAddr { address, .. } => {
                println!("Listening on {address}");
            }
            SwarmEvent::Behaviour(RelayBehaviourEvent::Relay(
                relay::Event::ReservationReqAccepted { src_peer_id, .. },
            )) => {
                tracing::info!("relay reservation from {src_peer_id}");
            }
            _ => {}
        }
    }
}