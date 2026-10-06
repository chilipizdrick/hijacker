pub mod args;
pub mod audio_source;
pub mod client;
pub mod notify;
pub mod subcommands;
pub mod types;

use anyhow::Context;

use crate::{
    args::{CliArgs, Command},
    client::PipewireClient,
};

pub fn run(args: CliArgs) -> anyhow::Result<()> {
    let client = PipewireClient::new().context("failed to initialize pipewire client")?;

    match args.command {
        Command::Play(args) => subcommands::play(&client, args),
        Command::CleverPlay(args) => subcommands::clever_play(&client, args),
        Command::Link(args) => subcommands::link(&client, args),
    }
}
