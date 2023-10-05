use std::error::Error;
use std::net::SocketAddr;
use std::str::FromStr;
use std::time::Instant;

use clap::Parser;
use futures::{future::Either, StreamExt};
use libp2p::{
    core::{
        multiaddr::Protocol, muxing::StreamMuxerBox, transport::OrTransport, upgrade, Multiaddr,
    },
    dns, identity, quic,
    swarm::{NetworkBehaviour, Swarm, SwarmBuilder, SwarmEvent},
    tcp, tls, yamux, PeerId, Transport as _,
};
use libp2p_perf::{Run, RunDuration, RunParams};
use log::{error, info};

#[derive(Debug, Parser)]
#[clap(name = "libp2p perf client")]
struct Opts {
    #[arg(long)]
    server_address: Option<SocketAddr>,
    #[arg(long)]
    transport: Option<Transport>,
    #[arg(long)]
    upload_bytes: Option<usize>,
    #[arg(long)]
    download_bytes: Option<usize>,

    /// Run in server mode.
    #[clap(long)]
    run_server: bool,
}

/// Supported transports by rust-libp2p.
#[derive(Clone, Debug)]
pub enum Transport {
    Tcp,
    QuicV1,
}

impl FromStr for Transport {
    type Err = String;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "tcp" => Ok(Self::Tcp),
            "quic-v1" => Ok(Self::QuicV1),
            _ => Err("Expected either 'tcp' or 'quic-v1'".to_string()),
        }
    }
}

#[async_std::main]
async fn main() -> Result<(), Box<dyn Error>> {
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info"))
        .format_timestamp_millis()
        .init();

    let opts = Opts::parse();
    match opts {
        Opts {
            server_address: Some(server_address),
            transport: None,
            upload_bytes: None,
            download_bytes: None,
            run_server: true,
        } => server(server_address).await?,
        Opts {
            server_address: Some(server_address),
            transport: Some(transport),
            upload_bytes: Some(upload_bytes),
            download_bytes: Some(download_bytes),
            run_server: false,
        } => {
            client(server_address, transport, upload_bytes, download_bytes).await?;
        }
        _ => panic!("invalid command line arguments: {opts:?}"),
    };

    Ok(())
}

async fn server(server_address: SocketAddr) -> Result<(), Box<dyn Error>> {
    let mut swarm = swarm::<libp2p_perf::server::Behaviour>().await?;

    swarm.listen_on(
        Multiaddr::empty()
            .with(server_address.ip().into())
            .with(Protocol::Tcp(server_address.port())),
    )?;

    swarm
        .listen_on(
            Multiaddr::empty()
                .with(server_address.ip().into())
                .with(Protocol::Udp(server_address.port()))
                .with(Protocol::QuicV1),
        )
        .unwrap();

    loop {
        match swarm.next().await.unwrap() {
            SwarmEvent::NewListenAddr { address, .. } => {
                info!("Listening on {address}");
            }
            SwarmEvent::IncomingConnection { .. } => {}
            e @ SwarmEvent::IncomingConnectionError { .. } => {
                error!("{e:?}");
            }
            SwarmEvent::ConnectionEstablished {
                peer_id, endpoint, ..
            } => {
                info!("Established connection to {:?} via {:?}", peer_id, endpoint);
            }
            SwarmEvent::ConnectionClosed { .. } => {}
            SwarmEvent::Behaviour(()) => {
                info!("Finished run",)
            }
            e => panic!("{e:?}"),
        }
    }
}

async fn client(
    server_address: SocketAddr,
    transport: Transport,
    upload_bytes: usize,
    download_bytes: usize,
) -> Result<(), Box<dyn Error>> {
    let server_address = match transport {
        Transport::Tcp => Multiaddr::empty()
            .with(server_address.ip().into())
            .with(Protocol::Tcp(server_address.port())),
        Transport::QuicV1 => Multiaddr::empty()
            .with(server_address.ip().into())
            .with(Protocol::Udp(server_address.port()))
            .with(Protocol::QuicV1),
    };

    custom(
        server_address,
        RunParams {
            to_send: upload_bytes,
            to_receive: download_bytes,
        },
    )
    .await?;

    Ok(())
}

async fn custom(server_address: Multiaddr, params: RunParams) -> Result<(), Box<dyn Error>> {
    info!("start benchmark: custom");
    let mut swarm = swarm().await?;

    let start = Instant::now();

    let server_peer_id = connect(&mut swarm, server_address.clone()).await?;

    perf(&mut swarm, server_peer_id, params).await?;

    info!(
        "end benchmark: custom ({} s)",
        start.elapsed().as_secs_f64()
    );

    Ok(())
}

async fn swarm<B: NetworkBehaviour + Default>() -> Result<Swarm<B>, Box<dyn Error>> {
    let local_key = identity::Keypair::generate_ed25519();
    let local_peer_id = PeerId::from(local_key.public());

    let transport = {
        let tcp = tcp::async_io::Transport::new(tcp::Config::default().nodelay(true))
            .upgrade(upgrade::Version::V1Lazy)
            .authenticate(tls::Config::new(&local_key)?)
            .multiplex(yamux::Config::default());

        let quic = {
            let mut config = quic::Config::new(&local_key);
            config.support_draft_29 = true;
            quic::async_std::Transport::new(config)
        };

        let dns = dns::DnsConfig::system(OrTransport::new(quic, tcp)).await?;

        dns.map(|either_output, _| match either_output {
            Either::Left((peer_id, muxer)) => (peer_id, StreamMuxerBox::new(muxer)),
            Either::Right((peer_id, muxer)) => (peer_id, StreamMuxerBox::new(muxer)),
        })
        .boxed()
    };

    Ok(
        SwarmBuilder::with_async_std_executor(transport, Default::default(), local_peer_id)
            .substream_upgrade_protocol_override(upgrade::Version::V1Lazy)
            .build(),
    )
}

async fn connect(
    swarm: &mut Swarm<libp2p_perf::client::Behaviour>,
    server_address: Multiaddr,
) -> Result<PeerId, Box<dyn Error>> {
    let start = Instant::now();
    swarm.dial(server_address.clone()).unwrap();

    let server_peer_id = match swarm.next().await.unwrap() {
        SwarmEvent::ConnectionEstablished { peer_id, .. } => peer_id,
        SwarmEvent::OutgoingConnectionError { peer_id, error, .. } => {
            return Err(format!("Outgoing connection error to {:?}: {:?}", peer_id, error).into());
        }
        e => panic!("{e:?}"),
    };

    let duration = start.elapsed();
    let duration_seconds = duration.as_secs_f64();

    info!("established connection in {duration_seconds:.4} s");

    Ok(server_peer_id)
}

async fn perf(
    swarm: &mut Swarm<libp2p_perf::client::Behaviour>,
    server_peer_id: PeerId,
    params: RunParams,
) -> Result<RunDuration, Box<dyn Error>> {
    swarm.behaviour_mut().perf(server_peer_id, params)?;

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
