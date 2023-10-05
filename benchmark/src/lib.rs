use std::error::Error;

use clap::ValueEnum;
use futures::{FutureExt, StreamExt};
use libp2p::identity::Keypair;
use libp2p::swarm::NetworkBehaviour;
use libp2p::swarm::Swarm;
use libp2p::swarm::SwarmEvent;
use log::info;

#[derive(Clone, Debug, ValueEnum)]
pub enum TransportMethod {
    Tcp,
    QuicV1,
}

pub fn generate_ed25519(seed: u8) -> Keypair {
    let mut bytes = [0u8; 32];
    bytes[0] = seed;

    Keypair::ed25519_from_bytes(bytes).expect("only errors on wrong length")
}

pub async fn swarm_listen<B: NetworkBehaviour>(
    swarm: &mut Swarm<B>,
    transport: TransportMethod,
) -> Result<(), Box<dyn Error>> 
    where <B as NetworkBehaviour>::ToSwarm: std::fmt::Debug 
{
    let listen_address = match transport {
        TransportMethod::Tcp => "/ip4/0.0.0.0/tcp/0".parse()?,
        TransportMethod::QuicV1 => "/ip4/0.0.0.0/udp/0/quic-v1".parse()?,
    };
    swarm.listen_on(listen_address)?;

    // Wait to listen on all interfaces.
    let mut delay = futures_timer::Delay::new(std::time::Duration::from_secs(1)).fuse();
    loop {
        futures::select! {
            event = swarm.next() => {
                match event.unwrap() {
                    SwarmEvent::NewListenAddr { address, .. } => {
                        info!("Listening on {}", address);
                    }
                    event => panic!("{event:?}"),
                }
            }
            _ = delay => {
                // Likely listening on all interfaces now so break loop and continue.
                return Ok(());
            }
        }
    }
}
