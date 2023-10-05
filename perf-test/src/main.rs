use clap::Parser;
use futures::{
    future::{Either, FutureExt},
    stream::StreamExt,
};
use libp2p::{
    core::{
        multiaddr::{Multiaddr, Protocol},
        muxing::StreamMuxerBox,
        transport::Transport,
        upgrade,
    },
    dcutr, dns, identify, identity, noise, ping, quic, relay,
    perf::{Run, RunDuration, RunParams},
    swarm::{NetworkBehaviour, Swarm, SwarmBuilder, SwarmEvent},
    tcp, yamux, PeerId,
};
use log::info;
use std::error::Error;
use std::str::FromStr;

#[derive(Debug, Parser)]
#[command(author, version, about, long_about = None)]
struct Args {
    /// The mode (client-listen, client-dial).
    #[arg(short, long)]
    mode: Mode,

    /// Fixed value to generate deterministic peer id.
    #[arg(short, long)]
    secret_key_seed: u8,

    /// The listening address
    #[arg(long)]
    relay_address: Multiaddr,

    /// Peer ID of the remote peer to hole punch to.
    #[arg(long)]
    remote_peer_id: Option<PeerId>,

    // TODO: add transports option for TCP vs QUIC
    // https://github.com/libp2p/rust-libp2p/blob/7d1d67cad3847a845ad50d9e56b3b68ca53f5e22/protocols/perf/src/bin/perf.rs#L44
}

#[derive(Clone, Debug, PartialEq, Parser)]
enum Mode {
    Dial,
    Listen,
}

impl FromStr for Mode {
    type Err = String;
    fn from_str(mode: &str) -> Result<Self, Self::Err> {
        match mode {
            "dial" => Ok(Mode::Dial),
            "listen" => Ok(Mode::Listen),
            _ => Err("Expected either 'dial' or 'listen'".to_string()),
        }
    }
}

#[derive(NetworkBehaviour)]
struct ClientBehaviour {
    relay_client: relay::client::Behaviour,
    ping: ping::Behaviour,
    identify: identify::Behaviour,
    dcutr: dcutr::Behaviour,
    perf: libp2p_perf::client::Behaviour,
}

#[derive(NetworkBehaviour)]
struct ServerBehaviour {
    relay_client: relay::client::Behaviour,
    ping: ping::Behaviour,
    identify: identify::Behaviour,
    dcutr: dcutr::Behaviour,
    perf: libp2p_perf::server::Behaviour,
}

#[async_std::main]
async fn main() -> Result<(), Box<dyn Error>> {
    env_logger::init();

    let args = Args::parse();

    let mut swarm = create_swarm(args.secret_key_seed).await.unwrap();

    swarm
        .listen_on("/ip4/0.0.0.0/udp/0/quic-v1".parse().unwrap())
        .unwrap();
    swarm
        .listen_on("/ip4/0.0.0.0/tcp/0".parse().unwrap())
        .unwrap();

    // Wait to listen on all interfaces.
    let mut delay = futures_timer::Delay::new(std::time::Duration::from_secs(1)).fuse();
    loop {
        futures::select! {
            event = swarm.next() => {
                match event.unwrap() {
                    SwarmEvent::NewListenAddr { address, .. } => {
                        info!("Listening on {:?}", address);
                    }
                    event => panic!("{event:?}"),
                }
            }
            _ = delay => {
                // Likely listening on all interfaces now, thus continuing by breaking the loop.
                break;
            }
        }
    }

    // Connect to the relay server. Not for the reservation or relayed
    // connection, but to (a) learn our local public address and (b) enable
    // a freshly started relay to learn its public address.
    swarm.dial(args.relay_address.clone()).unwrap();
    let mut learned_observed_addr = false;
    let mut told_relay_observed_addr = false;

    loop {
        match swarm.select_next_some().await {
            SwarmEvent::NewListenAddr { .. } => {}
            SwarmEvent::Dialing { .. } => {}
            SwarmEvent::ConnectionEstablished { .. } => {}
            SwarmEvent::Behaviour(BehaviourEvent::Ping(_)) => {}
            SwarmEvent::Behaviour(BehaviourEvent::Identify(identify::Event::Sent { .. })) => {
                info!("Told relay its public address.");
                told_relay_observed_addr = true;
            }
            SwarmEvent::Behaviour(BehaviourEvent::Identify(identify::Event::Received {
                info: identify::Info { observed_addr, .. },
                ..
            })) => {
                info!("Relay told us our public address: {:?}", observed_addr);
                swarm.add_external_address(observed_addr);
                learned_observed_addr = true;
            }
            event => panic!("{event:?}"),
        }

        if learned_observed_addr && told_relay_observed_addr {
            break;
        }
    }

    match args.mode {
        Mode::Dial => {
            swarm
                .dial(
                    args.relay_address
                        .with(Protocol::P2pCircuit)
                        .with(Protocol::P2p(args.remote_peer_id.unwrap())),
                )
                .unwrap();
        }
        Mode::Listen => {
            swarm
                .listen_on(args.relay_address.with(Protocol::P2pCircuit))
                .unwrap();
        }
    }

    loop {
        match swarm.select_next_some().await {
            SwarmEvent::NewListenAddr { address, .. } => {
                info!("Listening on {:?}", address);
            }
            SwarmEvent::Behaviour(BehaviourEvent::RelayClient(
                relay::client::Event::ReservationReqAccepted { .. },
            )) => {
                assert!(args.mode == Mode::Listen);
                info!("Relay accepted our reservation request.");
            }
            SwarmEvent::Behaviour(BehaviourEvent::RelayClient(event)) => {
                info!("{:?}", event)
            }
            SwarmEvent::Behaviour(BehaviourEvent::Dcutr(event)) => {
                info!("{:?}", event)
            }
            SwarmEvent::Behaviour(BehaviourEvent::Identify(event)) => {
                info!("{:?}", event)
            }
            SwarmEvent::Behaviour(BehaviourEvent::Ping(_)) => {}
            SwarmEvent::Behaviour(()) => {
                info!("Finished benchmark run with peer")
            }
            SwarmEvent::ConnectionEstablished {
                peer_id, endpoint, ..
            } => {
                info!("Established connection to {:?} via {:?}", peer_id, endpoint);
                if args.mode == Mode::Dial {
                    info!("Initiating benchmark");
                    benchmark(
                        &mut swarm,
                        peer_id,
                        args.upload_bytes.unwrap(),
                        args.download_bytes.unwrap(),
                    ).await?;
                }
            }
            SwarmEvent::OutgoingConnectionError { peer_id, error, .. } => {
                info!("Outgoing connection error to {:?}: {:?}", peer_id, error);
            }
            _ => {}
        }
    }
}

async fn create_swarm(secret_key_seed: u8) -> Result<Swarm<Behaviour>, Box<dyn Error>> {
    let local_key = generate_ed25519(secret_key_seed);
    let local_peer_id = PeerId::from(local_key.public());

    let (relay_transport, client) = relay::client::new(local_peer_id);

    let transport = {
        let relay_tcp_quic_transport = relay_transport
            .or_transport(tcp::async_io::Transport::new(
                tcp::Config::default().port_reuse(true),
            ))
            .upgrade(upgrade::Version::V1)
            .authenticate(noise::Config::new(&local_key).unwrap())
            .multiplex(yamux::Config::default())
            .or_transport(quic::async_std::Transport::new(quic::Config::new(
                &local_key,
            )));

        dns::DnsConfig::system(relay_tcp_quic_transport)
            .await?
            .map(|either_output, _| match either_output {
                Either::Left((peer_id, muxer)) => (peer_id, StreamMuxerBox::new(muxer)),
                Either::Right((peer_id, muxer)) => (peer_id, StreamMuxerBox::new(muxer)),
            })
            .boxed()
    };

    let behaviour = Behaviour {
        relay_client: client,
        ping: ping::Behaviour::new(ping::Config::new()),
        identify: identify::Behaviour::new(identify::Config::new(
            "/TODO/0.0.1".to_string(),
            local_key.public(),
        )),
        dcutr: dcutr::Behaviour::new(local_peer_id),
    };

    Ok(SwarmBuilder::with_async_std_executor(transport, behaviour, local_peer_id).build())
}

async fn benchmark(
    swarm: &mut Swarm<libp2p_perf::client::Behavior>,
    remote_peer_id: PeerId,
    upload_bytes: u8,
    download_bytes: u8,
) -> Result<RunDuration> {
    let params = RunParams { to_send: upload_bytes, to_receive: download_bytes };

    swarm.behaviour_mut().perf(remote_peer_id, params)?;

    let duration = match swarm.next().await.unwrap() {
        SwarmEvent::Behaviour(libp2p_perf::client::Event {
            id: _,
            result: Ok(duration),
        }) => duration,
        e => panic!("{e:?}"),
    };

    info!("{}", Run { params, duration });

    Ok(duration)
}

fn generate_ed25519(secret_key_seed: u8) -> identity::Keypair {
    let mut bytes = [0u8; 32];
    bytes[0] = secret_key_seed;

    identity::Keypair::ed25519_from_bytes(bytes).expect("only errors on wrong length")
}
