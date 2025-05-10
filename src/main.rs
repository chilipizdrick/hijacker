mod audio_player;
mod error;
mod pipewire;

use clap::{Parser, arg};
use pipewire::PortDirection;

use crate::audio_player::{AsyncAudioPlayer, AudioPlayer};
pub use crate::{
    error::{Error, Result},
    pipewire::{LinkInfo, Node},
};

#[derive(Parser, Debug)]
#[command(author, version, about = "Play audio to microphone", long_about = None)]
struct Args {
    #[arg(help = "Path to the audio file to play")]
    audio_file: String,

    #[arg(
        short,
        long,
        default_value = "1.0",
        help = "Volume multiplier: 1.0 is equivalent to 100%"
    )]
    volume: f32,
}

fn main() -> Result<()> {
    tracing_subscriber::fmt::init();
    let args = Args::parse();

    // Kind of a shenanigan tbh
    let self_executable_path = std::env::current_exe().map_err(Error::IO)?;
    let self_executable_name = self_executable_path
        .iter()
        .next_back()
        .ok_or(Error::BinaryNameUnset)?
        .to_string_lossy();

    let pw_client = pipewire::Client::new();

    let player = AudioPlayer::try_new()?;
    player.set_volume(args.volume);

    let application_nodes = pw_client.application_nodes()?;

    let self_node = application_nodes
        .iter()
        .find(|node| {
            node.application_name()
                .is_some_and(|name| name.contains(self_executable_name.as_ref()))
        })
        .cloned()
        .ok_or(Error::ApplicationNodeNotFound)?;

    let application_node_ids: Vec<_> = application_nodes.iter().map(|node| node.id()).collect();

    let ports = pw_client.ports()?;
    let application_input_ports: Vec<_> = ports
        .iter()
        .filter(|port| {
            application_node_ids.contains(&port.node_id())
                && matches!(port.direction(), PortDirection::In)
        })
        .collect();

    let self_output_port = ports
        .iter()
        .find(|port| port.node_id() == self_node.id())
        .ok_or(Error::ApplicationOutputPortNotFound)?;

    let links = application_input_ports
        .iter()
        .map(|port| {
            let link_info = LinkInfo::new(
                self_output_port.node_id(),
                self_output_port.port_id(),
                port.node_id(),
                port.port_id(),
            );
            pw_client.create_link(link_info)
        })
        .collect::<Result<Vec<_>>>()?;

    player.play_audio_file(args.audio_file)?;
    player.sleep_until_end();

    for link in links {
        pw_client.remove_link(link)?;
    }

    pw_client.quit()?;

    Ok(())
}
