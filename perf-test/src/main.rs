use futures::prelude::*;
use clap::Parser;
use libp2p::swarm::{keep_alive, NetworkBehaviour, SwarmEvent, SwarmBuilder};
use libp2p::{
    identity,
    Multiaddr,
    ping,
    PeerId,
};
use log::info;
use std::error::Error;
use std::str::FromStr;

#[derive(Parser, Debug)]
#[command(author, version, about, long_about = None)]
struct Args {
    #[arg(short, long)]
    mode: Mode,

    #[arg(short, long)]
    secret_key_seed: u8,

    #[arg(short, long)]
    remote_multiaddr: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Parser)]
enum Mode {
    Client,
    Server,
}

impl FromStr for Mode {
    type Err = String;
    fn from_str(mode: &str) -> Result<Self, Self::Err> {
        match mode {
            "client" => Ok(Mode::Client),
            "server" => Ok(Mode::Server),
            _ => Err("Expected either 'client' or 'server'".to_string()),
        }
    }
}

#[async_std::main]
async fn main() -> Result<(), Box<dyn Error>> {
    env_logger::init();

    let args = Args::parse();

    let local_key = identity::Keypair::generate_ed25519();
    let local_peer_id = PeerId::from(local_key.public());

    info!("{:?} starting with peer id {:?}", args.mode, local_peer_id);

    let transport = libp2p::development_transport(local_key).await?;

    let behaviour = Behaviour::default();

    let mut swarm = SwarmBuilder::with_async_std_executor(
        transport,
        behaviour,
        local_peer_id,
    ).build();

    // Tell the swarm to listen on all interfaces and a random port.
    swarm.listen_on("/ip4/0.0.0.0/tcp/0".parse()?)?;

    // Dial the peer identified by the multi-address given as the second
    // command-line argument, if any.
    if let Some(addr) = args.remote_multiaddr {
        let remote: Multiaddr = addr.parse()?;
        swarm.dial(remote)?;
        info!("Dialed {addr}")
    }

    loop {
        match swarm.select_next_some().await {
            SwarmEvent::NewListenAddr { address, .. } => info!("Listening on {address:?}"),
            SwarmEvent::Behaviour(event) => info!("{event:?}"),
            _ => {}
        }
    }

    Ok(())
}


#[derive(NetworkBehaviour, Default)]
struct Behaviour {
    keep_alive: keep_alive::Behaviour,
    ping: ping::Behaviour,
}
