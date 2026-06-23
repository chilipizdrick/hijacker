pub mod cli_args;
mod convertions;
mod pw_connection;
mod pw_objects;

pub use pipewire;

use anyhow::anyhow;
use pipewire::spa::utils::dict::ParsableValue;
use pipewire::types::ObjectType;
use ringbuf::HeapRb;
use ringbuf::traits::{Producer, Split};
use std::collections::HashSet;
use std::fs::File;
use std::path::Path;
use std::thread;
use std::time::Duration;
use symphonia::core::audio::SampleBuffer;
use symphonia::core::codecs::{CodecParameters, Decoder};
use symphonia::core::formats::FormatReader;
use symphonia::core::io::MediaSourceStream;

use crate::cli_args::Args;
use crate::pw_connection::PipewireConnection;
use crate::pw_objects::{DefaultMetadata, NodeWithPorts, Port, PwObject};

/// Relies on user to call `pipewire::init()` before and `pipewire::deinit()` after calling this function.
pub fn run(args: Args) -> anyhow::Result<()> {
    let (decoder, format_reader) = decode_audio_file(args.file_path)?;

    let connection = PipewireConnection::new()?;

    let codec_params = decoder.codec_params();
    let num_channels = channel_count(codec_params)
        .ok_or(anyhow!("Missing audio channel data in codec parameters"))?;

    // NOTE: The stream should be created before we collect the objects from the server. Otherwise
    // the stream's ports will not be available, unless we recreate connection.
    let stream = connection.create_and_connect_output_stream(codec_params)?;

    let objects = connection.get_server_objects(&stream)?;
    log::debug!(
        "Found {} relevant objects on pipewire server",
        objects.len()
    );

    let source_name = match args.audio_source_name {
        Some(name) => name,
        None => {
            let default_metadata = connection
                .find_default_metadata_object(&objects)?
                .ok_or(anyhow!("Could not get default metadata on pipewire server"))?;

            let name = select_audio_source_name(&default_metadata).ok_or(anyhow!(
                "Pipewire metadata object does not contain any default audio source"
            ))?;

            name.to_string()
        }
    };
    log::info!("Using reference audio source node with name `{source_name}`");

    let source_node = find_source_node(&objects, &source_name).ok_or(anyhow!(
        "Could not find audio source node with name `{source_name}`"
    ))?;
    log::debug!("Found audio source node (id: {})", source_node.id);

    let source_adjacent_links = node_adjacent_links(&objects, source_node);
    log::debug!("Found following links connected to source node: {source_adjacent_links:?}");

    let node_ids_to_connect_to: Vec<_> = link_adjacent_input_node_ids(&source_adjacent_links)
        .into_iter()
        .collect();
    log::debug!("Nodes to connect to: {node_ids_to_connect_to:?}");

    let ports: Vec<_> = objects
        .iter()
        .filter(|&o| o.type_ == ObjectType::Port)
        .collect();

    let connection_targets = connection_targets(&ports, &node_ids_to_connect_to);
    log::debug!("Found following connection targets: {connection_targets:?}");

    let stream_node_id = stream.node_id();

    let stream_ports = node_audio_ports(&ports, stream_node_id);
    log::debug!("Stream ports: {stream_ports:?}");

    let stream_node = NodeWithPorts::new(stream_node_id, stream_ports);

    connection.create_links_from_source_to_nodes(&stream_node, &connection_targets)?;

    let audio_buffer = HeapRb::<f32>::new(32768 * num_channels as usize);
    let (producer, consumer) = audio_buffer.split();

    let _stream_listener =
        connection.register_stream_proccessing_listener(&stream, consumer, num_channels)?;
    play_audio_into_ring_buffer(producer, decoder, format_reader);
    stream.set_active(true)?;

    connection.sync()?;
    connection.run_mainloop();

    Ok(())
}

fn channel_count(params: &CodecParameters) -> Option<u32> {
    params.channels.map(|c| c.bits().count_ones())
}

fn find_source_node<'a>(objects: &'a [PwObject], source_name: &str) -> Option<&'a PwObject> {
    objects.iter().find(|o| {
        is_of_type_and_has_str_property_equal_to(o, ObjectType::Node, "node.name", source_name)
    })
}

fn node_adjacent_links<'a>(
    objects: &'a [PwObject],
    source_node: &'a PwObject,
) -> Vec<&'a PwObject> {
    objects
        .iter()
        .filter(|&o| {
            is_of_type_and_has_property_equal_to(
                o,
                ObjectType::Link,
                "link.output.node",
                source_node.id,
            )
        })
        .collect()
}

fn is_of_type_and_has_property_equal_to<T>(
    object: &PwObject,
    type_: ObjectType,
    prop: &str,
    value: T,
) -> bool
where
    T: PartialEq + ParsableValue,
{
    object.type_ == type_
        && object
            .props
            .as_ref()
            .is_some_and(|p| p.dict().parse(prop) == Some(Ok(value)))
}

fn is_of_type_and_has_str_property_equal_to(
    object: &PwObject,
    type_: ObjectType,
    prop: &str,
    value: &str,
) -> bool {
    object.type_ == type_
        && object
            .props
            .as_ref()
            .is_some_and(|p| p.get(prop) == Some(value))
}

fn link_adjacent_input_node_ids(links: &[&PwObject]) -> HashSet<u32> {
    links
        .iter()
        .filter_map(|&o| {
            o.props
                .as_ref()
                .and_then(|p| p.dict().parse::<u32>("link.input.node"))
        })
        .flatten()
        .collect()
}

fn connection_targets(ports: &[&PwObject], node_ids: &[u32]) -> Vec<NodeWithPorts> {
    node_ids
        .iter()
        .map(|&node_id| {
            let ports = node_input_audio_ports(ports, node_id);
            NodeWithPorts::new(node_id, ports)
        })
        .collect()
}

fn node_input_audio_ports(ports: &[&PwObject], node_id: u32) -> Vec<Port> {
    let mut result = Vec::new();

    for port in ports {
        if let Some(props) = port.props.as_ref() {
            let port_node_id = props.dict().parse::<u32>("node.id");
            let port_direction = props.get("port.direction");
            if port_node_id == Some(Ok(node_id)) && port_direction == Some("in") {
                let audio_channel = props.get("audio.channel").unwrap_or("MONO").to_string();
                result.push(Port::new(port.id, audio_channel));
            }
        }
    }

    result
}

fn node_audio_ports(ports: &[&PwObject], node_id: u32) -> Vec<Port> {
    ports
        .iter()
        .filter(|&o| is_of_type_and_has_property_equal_to(o, ObjectType::Port, "node.id", node_id))
        .map(|&o| {
            let audio_channel = o
                .props
                .as_ref()
                .and_then(|p| p.get("audio.channel"))
                .unwrap_or("MONO")
                .to_string();
            Port::new(o.id, audio_channel)
        })
        .collect()
}

fn is_default_metadata_object(object: &PwObject) -> bool {
    is_of_type_and_has_str_property_equal_to(
        object,
        ObjectType::Metadata,
        "metadata.name",
        "default",
    )
}

fn play_audio_into_ring_buffer<P>(
    mut producer: P,
    mut decoder: Box<dyn Decoder + 'static>,
    mut format_reader: Box<dyn FormatReader + 'static>,
) where
    P: Producer<Item = f32> + Send + 'static,
{
    thread::spawn(move || {
        let mut sample_buf = None;
        while let Ok(packet) = format_reader.next_packet() {
            let decoded = decoder
                .decode(&packet)
                .expect("Could not decode audio packet");

            if sample_buf.is_none() {
                let spec = *decoded.spec();
                sample_buf = Some(SampleBuffer::new(decoded.capacity() as u64, spec));
            }

            if let Some(ref mut buf) = sample_buf {
                buf.copy_interleaved_ref(decoded);

                let samples = buf.samples();
                let mut current_sample_idx = 0;
                let samples_len = samples.len();

                while current_sample_idx < samples_len {
                    let new_sample_idx =
                        (current_sample_idx + producer.vacant_len()).min(samples_len);
                    producer.push_slice(&samples[current_sample_idx..new_sample_idx]);
                    current_sample_idx = new_sample_idx;
                    if current_sample_idx < samples_len {
                        thread::sleep(Duration::from_millis(10));
                    }
                }
            }
        }
    });
}

fn decode_audio_file(
    file_path: impl AsRef<Path>,
) -> symphonia::core::errors::Result<(Box<dyn Decoder>, Box<dyn FormatReader>)> {
    let audio_source_file = File::open(&file_path)?;
    let mss = MediaSourceStream::new(Box::new(audio_source_file), Default::default());

    let hint = Default::default();
    let format_ops = Default::default();
    let metadata_ops = Default::default();
    let format_reader = symphonia::default::get_probe()
        .format(&hint, mss, &format_ops, &metadata_ops)?
        .format;

    let track = &format_reader.tracks()[0];
    let decoder =
        symphonia::default::get_codecs().make(&track.codec_params, &Default::default())?;

    Ok((decoder, format_reader))
}

fn select_audio_source_name(metadata: &DefaultMetadata) -> Option<&String> {
    metadata
        .default_audio_source_name
        .as_ref()
        .or(metadata.default_configured_audio_source_name.as_ref())
}
