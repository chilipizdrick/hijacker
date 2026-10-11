use std::{collections::HashSet, time::Duration};

use anyhow::{Context, anyhow};
use log::debug;

use crate::{
    args::{LinkArgs, PlayArgs},
    audio_source::AudioFileStream,
    client::PipewireClient,
    types::{LinkToggleResult, ModelSnapshot, NodeId, NodeModel},
};

pub fn play(client: &PipewireClient, args: PlayArgs) -> anyhow::Result<()> {
    let buffer_size_millis = args.audio_ring_buffer_size_millis.unwrap_or(2000);
    let audio_source =
        AudioFileStream::new(&args.file_path, Duration::from_millis(buffer_size_millis))
            .with_context(|| format!("failed to load audio file: {}", args.file_path))?;
    debug!("created audio source: {audio_source:?}");

    let stream = client
        .stream_builder(Box::new(audio_source))
        .with_name("hijacker")
        .with_autoconnect(true)
        .with_autoplay(false)
        .build()
        .context("failed to build playback stream")?;
    debug!("created stream with node ID: {:?}", stream.node_id);

    let snapshot = client.snapshot();
    debug!("got default metadata: {:?}", snapshot.metadata);

    let source_node_name = match args.source_node_name {
        Some(name) => name,
        None => snapshot
            .metadata
            .default_configured_audio_source
            .as_deref()
            .or(snapshot.metadata.default_audio_source.as_deref())
            .ok_or_else(|| anyhow!("pipewire metadata does not contain a default audio source"))?
            .to_string(),
    };
    debug!("selected source node name: {source_node_name}");

    let potential_nodes = snapshot.filter_nodes_by_name(&source_node_name);
    let source_node = potential_nodes
        .into_iter()
        .find(|n| n.is_audio_source() || n.media_class.is_none())
        .ok_or_else(|| {
            anyhow!("could not find audio source node with name `{source_node_name}`")
        })?;
    debug!("found reference source node: {source_node:?}");

    let downstream_links = snapshot.downstream_links(source_node.id);
    debug!("found {} downstream link(s)", downstream_links.len());

    let target_node_ids: HashSet<_> = downstream_links.iter().map(|l| l.input_node).collect();
    debug!("target consumer node IDs: {target_node_ids:?}");

    let mut total_links = 0;
    for target_node_id in target_node_ids {
        let created = client.link_nodes(stream.node_id, target_node_id, false)?;
        total_links += created.len();
    }
    debug!("mirrored {total_links} total audio link(s) to downstream consumers");

    stream.play();
    debug!("started stream playback");

    debug!("waiting for stream playback to finish");
    stream.wait_until_finished();
    debug!("stream playback finished");

    Ok(())
}

/// Like using `play`, but tries to use the provided node as default "microphone", or falls back to regular play.
/// Particularly useful when using EasyEffects, or other real-time audio processing software, which is not running all the time
pub fn clever_play(client: &PipewireClient, args: PlayArgs) -> anyhow::Result<()> {
    let snapshot = client.snapshot();

    if let Some(node_name) = &args.source_node_name
        && let Some(node) = snapshot.filter_nodes_by_name(node_name).first()
        && node.is_audio_source()
    {
        debug!("playing with configured audio source as reference");
        play(client, args)
    } else {
        let args = PlayArgs {
            source_node_name: None,
            ..args
        };

        debug!("playing with default audio source as reference");
        play(client, args)
    }
}

pub fn link(client: &PipewireClient, args: LinkArgs) -> anyhow::Result<()> {
    let snapshot = client.snapshot();

    let source_node = resolve_node(
        &snapshot,
        &args.source_node_name,
        |n| n.is_audio_source() || n.is_stream_output() || n.media_class.is_none(),
        "source (output)",
    )?;
    debug!("resolved source node: {source_node:?}");

    let target_node = resolve_node(
        &snapshot,
        &args.sink_node_name,
        |n| n.is_audio_sink() || n.is_stream_input() || n.media_class.is_none(),
        "target (input)",
    )?;
    debug!("resolved target node: {target_node:?}");

    let outcome = client.toggle_link_nodes(source_node.id, target_node.id, true)?;

    match &outcome {
        LinkToggleResult::Linked(ids) => {
            println!(
                "Linked '{}' -> '{}' across {} channel(s) (IDs: {:?})",
                source_node.name,
                target_node.name,
                ids.len(),
                ids.iter().map(|id| id.0).collect::<Vec<_>>()
            );
        }
        LinkToggleResult::Unlinked(count) => {
            println!(
                "Unlinked '{}' -X-> '{}' (removed {count} connection(s))",
                source_node.name, target_node.name
            );
        }
    }

    Ok(())
}

/// Helper to resolve a node by numeric ID first, then by name with role disambiguation
fn resolve_node<'a>(
    snapshot: &'a ModelSnapshot,
    identifier: &'a str,
    role_matcher: impl Fn(&NodeModel) -> bool,
    expected_role: &str,
) -> anyhow::Result<&'a NodeModel> {
    if let Ok(id) = identifier.parse::<u32>() {
        return snapshot
            .nodes
            .get(&NodeId(id))
            .ok_or_else(|| anyhow!("no node found with ID {id}"));
    }

    let candidates = snapshot.filter_nodes_by_name(identifier);

    if candidates.is_empty() {
        return Err(anyhow!("could not find any node named `{identifier}`"));
    }

    candidates
        .into_iter()
        .find(|n| role_matcher(n))
        .ok_or_else(|| {
            anyhow!(
                "found node(s) matching `{identifier}`, but none matches the expected role: {expected_role}"
            )
        })
}
