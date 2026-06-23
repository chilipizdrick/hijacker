use pipewire::{properties::PropertiesBox, registry::GlobalObject};

pub type PwObject = GlobalObject<PropertiesBox>;

#[derive(Debug, Default)]
pub struct DefaultMetadata {
    pub default_audio_source_name: Option<String>,
    pub default_configured_audio_source_name: Option<String>,
    pub default_audio_sink_name: Option<String>,
    pub default_configured_audio_sink_name: Option<String>,
}

#[derive(Debug, Clone)]
pub struct NodeWithPorts {
    pub id: u32,
    pub ports: Vec<Port>,
}

impl NodeWithPorts {
    pub fn new(id: u32, ports: Vec<Port>) -> Self {
        Self { id, ports }
    }
}

#[derive(Debug, Clone)]
pub struct Port {
    pub id: u32,
    pub audio_channel: String,
}

impl Port {
    pub fn new(id: u32, audio_channel: String) -> Self {
        Self { id, audio_channel }
    }
}

#[derive(Debug, Clone)]
pub struct Link {
    pub output_node: u32,
    pub output_port: u32,
    pub input_node: u32,
    pub input_port: u32,
}

impl Link {
    pub fn new(output_node: u32, output_port: u32, input_node: u32, input_port: u32) -> Self {
        Self {
            output_node,
            output_port,
            input_node,
            input_port,
        }
    }
}
