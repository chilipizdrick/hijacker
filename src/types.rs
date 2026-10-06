use std::{
    collections::HashMap,
    str::FromStr,
    sync::mpsc::{self, Receiver},
};

use crate::{
    audio_source::AudioSource,
    client::{Command, PWResult, PipewireClient},
};

macro_rules! id_type {
    ($type:ident) => {
        #[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
        pub struct $type(pub u32);
    };
}

id_type!(NodeId);
id_type!(PortId);
id_type!(LinkId);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Direction {
    Input,
    Output,
}

impl FromStr for Direction {
    type Err = ();
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "in" => Ok(Self::Input),
            "out" => Ok(Self::Output),
            _ => Err(()),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MediaClass {
    AudioSource,
    AudioSourceVirtual,
    AudioSink,
    AudioSinkVirtual,
    StreamInput,
    StreamOutput,
    Other(String),
}

impl<S: AsRef<str>> From<S> for MediaClass {
    fn from(s: S) -> Self {
        match s.as_ref() {
            "Audio/Source" => Self::AudioSource,
            "Audio/Source/Virtual" => Self::AudioSourceVirtual,
            "Audio/Sink" => Self::AudioSink,
            "Audio/Sink/Virtual" => Self::AudioSinkVirtual,
            "Stream/Input/Audio" => Self::StreamInput,
            "Stream/Output/Audio" => Self::StreamOutput,
            other => Self::Other(other.to_string()),
        }
    }
}

#[derive(Debug, Clone)]
pub struct NodeModel {
    pub id: NodeId,
    pub name: String,
    pub description: Option<String>,
    pub media_class: Option<MediaClass>,
    pub app_name: Option<String>,
    pub pid: Option<u32>,
}

impl NodeModel {
    pub fn is_audio_source(&self) -> bool {
        matches!(
            self.media_class,
            Some(MediaClass::AudioSource | MediaClass::AudioSourceVirtual)
        )
    }

    pub fn is_audio_sink(&self) -> bool {
        matches!(
            self.media_class,
            Some(MediaClass::AudioSink | MediaClass::AudioSinkVirtual)
        )
    }

    pub fn is_stream_input(&self) -> bool {
        matches!(self.media_class, Some(MediaClass::StreamInput))
    }

    pub fn is_stream_output(&self) -> bool {
        matches!(self.media_class, Some(MediaClass::StreamOutput))
    }
}

#[derive(Debug, Clone)]
pub struct PortModel {
    pub id: PortId,
    pub node_id: NodeId,
    pub direction: Direction,
    pub channel: String,
}

#[derive(Debug, Clone)]
pub struct LinkModel {
    pub id: LinkId,
    pub output_node: NodeId,
    pub output_port: PortId,
    pub input_node: NodeId,
    pub input_port: PortId,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LinkToggleResult {
    Linked(Vec<LinkId>),
    Unlinked(usize),
}

#[derive(Debug, Default, Clone)]
pub struct DefaultMetadata {
    pub default_audio_sink: Option<String>,
    pub default_audio_source: Option<String>,
    pub default_configured_audio_sink: Option<String>,
    pub default_configured_audio_source: Option<String>,
}

#[derive(Debug, Default, Clone)]
pub struct ModelSnapshot {
    pub nodes: HashMap<NodeId, NodeModel>,
    pub ports: HashMap<PortId, PortModel>,
    pub links: HashMap<LinkId, LinkModel>,
    pub node_ports: HashMap<NodeId, Vec<PortId>>,
    pub metadata: DefaultMetadata,
}

impl ModelSnapshot {
    pub fn filter_nodes_by_name<'a>(&'a self, name: &'a str) -> Vec<&'a NodeModel> {
        self.nodes
            .values()
            .filter(|n| n.name == name || n.description.as_deref() == Some(name))
            .collect()
    }

    pub fn filter_nodes_by_app<'a>(&'a self, app: &'a str) -> Vec<&'a NodeModel> {
        self.nodes
            .values()
            .filter(|n| n.app_name.as_deref() == Some(app))
            .collect()
    }

    pub fn ports_for_node(&self, node_id: NodeId, dir: Direction) -> Vec<&PortModel> {
        self.node_ports
            .get(&node_id)
            .map(|ids| {
                ids.iter()
                    .filter_map(|id| self.ports.get(id))
                    .filter(|p| p.direction == dir)
                    .collect()
            })
            .unwrap_or_default()
    }

    pub fn downstream_links(&self, source: NodeId) -> Vec<&LinkModel> {
        self.links
            .values()
            .filter(|l| l.output_node == source)
            .collect()
    }

    pub fn match_ports(&self, src: NodeId, dst: NodeId) -> Vec<(PortId, PortId)> {
        let out_ports = self.ports_for_node(src, Direction::Output);
        let in_ports = self.ports_for_node(dst, Direction::Input);

        let mut matches = Vec::new();
        for out in &out_ports {
            if let Some(matching_in) = in_ports.iter().find(|i| i.channel == out.channel) {
                matches.push((out.id, matching_in.id));
            }
        }

        if matches.is_empty() {
            for (out, inp) in out_ports.iter().zip(in_ports.iter()) {
                matches.push((out.id, inp.id));
            }
        }

        matches
    }

    pub fn links_between(&self, source: NodeId, target: NodeId) -> Vec<&LinkModel> {
        self.links
            .values()
            .filter(|l| l.output_node == source && l.input_node == target)
            .collect()
    }
}

#[derive(Debug, Clone)]
pub struct StreamConfig {
    pub name: String,
    pub autoconnect: bool,
    pub autoplay: bool,
}

impl Default for StreamConfig {
    fn default() -> Self {
        Self {
            name: "pipewire-playback".into(),
            autoconnect: true,
            autoplay: true,
        }
    }
}

pub struct StreamBuilder<'a> {
    client: &'a PipewireClient,
    source: Box<dyn AudioSource>,
    config: StreamConfig,
}

impl<'a> StreamBuilder<'a> {
    pub fn new(client: &'a PipewireClient, source: Box<dyn AudioSource>) -> Self {
        Self {
            client,
            source,
            config: StreamConfig::default(),
        }
    }

    pub fn with_name(mut self, name: impl Into<String>) -> Self {
        self.config.name = name.into();
        self
    }

    pub fn with_autoconnect(mut self, autoconnect: bool) -> Self {
        self.config.autoconnect = autoconnect;
        self
    }

    pub fn with_autoplay(mut self, autoplay: bool) -> Self {
        self.config.autoplay = autoplay;
        self
    }

    pub fn build(self) -> PWResult<ManagedStream> {
        let (finish_tx, finish_rx) = mpsc::channel();
        let config = self.config;
        let autoplay = config.autoplay;

        let node_id = self.client.send_cmd(|responder| Command::CreateStream {
            config,
            source: self.source,
            finish_tx,
            responder,
        })?;

        let stream = ManagedStream {
            node_id,
            command_tx: self.client.command_tx.clone(),
            finish_rx,
        };

        if !autoplay {
            stream.pause();
        }

        Ok(stream)
    }
}

pub struct ManagedStream {
    pub node_id: NodeId,
    command_tx: pipewire::channel::Sender<Command>,
    finish_rx: Receiver<()>,
}

impl ManagedStream {
    pub fn play(&self) {
        let _ = self.command_tx.send(Command::SetStreamState {
            node_id: self.node_id,
            playing: true,
        });
    }

    pub fn pause(&self) {
        let _ = self.command_tx.send(Command::SetStreamState {
            node_id: self.node_id,
            playing: false,
        });
    }

    pub fn wait_until_finished(&self) {
        let _ = self.finish_rx.recv();
    }
}

impl Drop for ManagedStream {
    fn drop(&mut self) {
        let _ = self.command_tx.send(Command::DestroyStream(self.node_id));
    }
}
