use clap::{Args, Parser, Subcommand};

#[derive(Debug, Clone, Parser)]
pub struct CliArgs {
    #[command(subcommand)]
    pub command: Command,
}

#[derive(Debug, Clone, Subcommand)]
pub enum Command {
    /// Play audio into all nodes connected to a provided or default audio source
    Play(PlayArgs),
    /// Link source node to sink node (preferrably preserving port channel)
    Link(LinkArgs),
    /// Like using `play`, but tries to use the provided node as default "microphone", or falls back to the configured default
    CleverPlay(PlayArgs),
}

#[derive(Debug, Clone, Args)]
pub struct LinkArgs {
    /// Name of pipewire source node
    #[arg(short = 's', long = "source")]
    pub source_node_name: String,
    /// Name of pipewire sink node
    #[arg(short = 't', long = "target")]
    pub sink_node_name: String,
}

#[derive(Debug, Clone, Args)]
pub struct PlayArgs {
    /// Path to the audio file to be played
    pub file_path: String,

    /// Name of the audio source node to use as the reference "microphone". If not specified, the default
    /// audio source node will be used.
    #[arg(short = 's', long = "source")]
    pub source_node_name: Option<String>,
}
