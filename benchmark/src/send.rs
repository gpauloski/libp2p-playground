use std::error::Error;

use clap::Parser;
use env_logger;
use futures::{future::Either, StreamExt};
use libp2p::{
    core::{
        multiaddr::{Multiaddr, Protocol},
        muxing::StreamMuxerBox,
        transport::Transport,
        upgrade,
    },
    dcutr, dns, identify, noise, ping, quic, relay,
    swarm::{NetworkBehaviour, Swarm, SwarmBuilder, SwarmEvent},
    tcp, yamux, PeerId,
};
use libp2p_perf::{Run, RunParams};
use log::info;

use benchmark::{generate_ed25519, swarm_listen, TransportMethod};

#[derive(Parser, Debug)]
#[command(author, version, about, long_about = None)]
struct Args {
    // Seed used to generate deterministic peer id.
    #[arg(short, long)]
    seed: u8,

    // Relay server multi-address.
    #[arg(short, long)]
    relay_multiaddr: Multiaddr,

    // Receiver peer ID.
    #[arg(long)]
    receiver_peer_id: PeerId,

    // Payload bytes.
    #[arg(long)]
    payload_bytes: usize,

    // Transport method (tcp or quic-v1).
    // Should match the transport method of relay_multiaddr.
    #[arg(short, long, value_enum, default_value_t=TransportMethod::Tcp)]
    transport: TransportMethod,
}

#[derive(NetworkBehaviour)]
struct Behaviour {
    relay_client: relay::client::Behaviour,
    ping: ping::Behaviour,
    identify: identify::Behaviour,
    dcutr: dcutr::Behaviour,
    perf: libp2p_perf::client::Behaviour,
}

#[async_std::main]
async fn main() -> Result<(), Box<dyn Error>> {
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info"))
        .format_timestamp_millis()
        .init();

    let args = Args::parse();

    info!("DCUTR Bandwidth Benchmark: Sender");
    info!("Relay multiaddr: {}", args.relay_multiaddr);
    info!("Transport method: {:?}", args.transport);

    let mut swarm = build_swarm(args.seed).await?;
    swarm_listen(&mut swarm, args.transport).await?;
    learn_external_address(&mut swarm, args.relay_multiaddr.clone()).await?;

    swarm
        .dial(
            args.relay_multiaddr
                .with(Protocol::P2pCircuit)
                .with(Protocol::P2p(args.receiver_peer_id)),
        )
        .unwrap();

    let params = RunParams {
        to_send: args.payload_bytes,
        to_receive: args.payload_bytes,
    };
    let mut started_benchmark = false;

    loop {
        match swarm.next().await.unwrap() {
            SwarmEvent::NewListenAddr { address, .. } => {
                info!("Listening on {:?}", address);
            }
            SwarmEvent::Behaviour(BehaviourEvent::RelayClient(
                relay::client::Event::ReservationReqAccepted { .. },
            )) => {
                panic!("Should only happen on receiver side.");
            }
            SwarmEvent::Behaviour(BehaviourEvent::RelayClient(event)) => {
                info!("{:?}", event)
            }
            SwarmEvent::Behaviour(BehaviourEvent::Dcutr(
                dcutr::Event::DirectConnectionUpgradeSucceeded { .. },
            )) => {
                info!("Direct connection upgrade successful!");
                if !started_benchmark {
                    swarm
                        .behaviour_mut()
                        .perf
                        .perf(args.receiver_peer_id, params)?;
                    started_benchmark = true;
                }
            }
            SwarmEvent::Behaviour(BehaviourEvent::Dcutr(
                dcutr::Event::DirectConnectionUpgradeFailed {
                    remote_peer_id: _,
                    error: e,
                },
            )) => {
                panic!("{e:?}")
            }
            SwarmEvent::Behaviour(BehaviourEvent::Dcutr(event)) => {
                info!("{:?}", event)
            }
            SwarmEvent::Behaviour(BehaviourEvent::Identify(event)) => {
                info!("{:?}", event)
            }
            SwarmEvent::Behaviour(BehaviourEvent::Ping(_)) => {}
            SwarmEvent::Behaviour(BehaviourEvent::Perf(libp2p_perf::client::Event {
                id: _,
                result: Ok(duration),
            })) => {
                assert!(started_benchmark, "Benchmark not started yet!");
                info!("Benchmark completed: {}", Run { params, duration });
                return Ok(());
            }
            SwarmEvent::ConnectionEstablished {
                peer_id, endpoint, ..
            } => {
                info!("Established connection to {:?} via {:?}", peer_id, endpoint);
            }
            SwarmEvent::OutgoingConnectionError { peer_id, error, .. } => {
                info!("Outgoing connection error to {:?}: {:?}", peer_id, error);
            }
            _ => {}
        }
    }
}

async fn build_swarm(seed: u8) -> Result<Swarm<Behaviour>, Box<dyn Error>> {
    let local_key = generate_ed25519(seed);
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
            .await
            .unwrap()
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
        perf: Default::default(),
    };

    Ok(SwarmBuilder::with_async_std_executor(transport, behaviour, local_peer_id).build())
}

async fn learn_external_address(
    swarm: &mut Swarm<Behaviour>,
    relay_address: Multiaddr,
) -> Result<(), Box<dyn Error>> {
    // Connect to the relay server. Not for the reservation or relayed
    // connection, but to (a) learn our local public address and (b) enable
    // a freshly started relay to learn its public address.
    swarm.dial(relay_address.clone())?;
    let mut learned_observed_addr = false;
    let mut told_relay_observed_addr = false;

    loop {
        match swarm.next().await.unwrap() {
            SwarmEvent::NewListenAddr { .. } => {}
            SwarmEvent::Dialing { .. } => {}
            SwarmEvent::ConnectionEstablished { .. } => {}
            SwarmEvent::Behaviour(BehaviourEvent::Ping(_)) => {}
            SwarmEvent::Behaviour(BehaviourEvent::Identify(identify::Event::Sent { .. })) => {
                info!("Notified relay of its public address");
                told_relay_observed_addr = true;
            }
            SwarmEvent::Behaviour(BehaviourEvent::Identify(identify::Event::Received {
                info: identify::Info { observed_addr, .. },
                ..
            })) => {
                info!("Relay says our public address is {}", observed_addr);
                swarm.add_external_address(observed_addr);
                learned_observed_addr = true;
            }
            event => panic!("{event:?}"),
        }

        if learned_observed_addr && told_relay_observed_addr {
            return Ok(());
        }
    }
}
