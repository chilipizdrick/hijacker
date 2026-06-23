use anyhow::anyhow;
use pipewire::properties::properties;
use pipewire::spa::param::audio::{AudioFormat, AudioInfoRaw};
use pipewire::spa::pod::Pod;
use pipewire::spa::pod::serialize::PodSerializer;
use pipewire::spa::sys::{SPA_PARAM_EnumFormat, SPA_TYPE_OBJECT_Format};
use pipewire::spa::utils::Direction;
use pipewire::stream::{StreamBox, StreamFlags, StreamState};
use pipewire::{self as pw, spa::pod};
use ringbuf::traits::Consumer;
use std::cell::RefCell;
use std::io::Cursor;
use std::rc::Rc;
use symphonia::core::codecs::CodecParameters;

use crate::convertions::{channels_to_spa_audio_channel, sample_format_to_audio_format};
use crate::pw_objects::{DefaultMetadata, Link, NodeWithPorts, PwObject};
use crate::{channel_count, is_default_metadata_object};

pub struct PipewireConnection {
    mainloop: pw::main_loop::MainLoopRc,
    core: pw::core::CoreRc,
    _context: pw::context::ContextRc,
    registry: pw::registry::RegistryRc,
}

impl PipewireConnection {
    pub fn new() -> Result<Self, pw::Error> {
        let mainloop = pw::main_loop::MainLoopRc::new(None)?;
        let context = pw::context::ContextRc::new(&mainloop, None)?;
        let core = context.connect_rc(None)?;
        let registry = core.get_registry_rc()?;

        Ok(Self {
            mainloop,
            core,
            _context: context,
            registry,
        })
    }

    pub fn create_link(&self, link: Link) -> Result<pw::link::Link, pw::Error> {
        self.core.create_object(
            "link-factory",
            &properties! {
                "link.output.node" => link.output_node.to_string(),
                "link.output.port" => link.output_port.to_string(),
                "link.input.node" => link.input_node.to_string(),
                "link.input.port" => link.input_port.to_string(),
                "object.linger" => "1",
            },
        )
    }

    pub fn create_and_connect_output_stream(
        &self,
        codec_params: &CodecParameters,
    ) -> anyhow::Result<pw::stream::StreamBox<'_>> {
        let num_channels = channel_count(codec_params)
            .ok_or(anyhow!("Missing audio channel data in codec parameters"))?;

        let props = properties! {
            *pw::keys::MEDIA_TYPE => "Audio",
            *pw::keys::MEDIA_CATEGORY => "Playback",
            *pw::keys::MEDIA_ROLE => "Music",
            *pw::keys::AUDIO_CHANNELS => num_channels.to_string(),
        };
        let stream = StreamBox::new(&self.core, "hijacker", props)?;

        let stream_flags = StreamFlags::MAP_BUFFERS | StreamFlags::AUTOCONNECT;

        let audio_info = audio_info_from_codec_parameters(codec_params)?;
        let object = pod::Object {
            type_: SPA_TYPE_OBJECT_Format,
            id: SPA_PARAM_EnumFormat,
            properties: audio_info.into(),
        };
        let pod_value = pod::Value::Object(object);
        let values: Vec<u8> = PodSerializer::serialize(Cursor::new(Vec::new()), &pod_value)?
            .0
            .into_inner();
        let mut params = [Pod::from_bytes(&values).ok_or(anyhow!("Could not serialize SPA pod"))?];

        stream.connect(Direction::Output, None, stream_flags, &mut params)?;

        Ok(stream)
    }

    pub fn register_registry_listener(
        &self,
        objects: &Rc<RefCell<Vec<PwObject>>>,
    ) -> pw::registry::Listener {
        let objects_ = objects.clone();
        self.registry
            .add_listener_local()
            .global(move |global| {
                use pw::types::ObjectType as OT;
                if matches!(global.type_, OT::Node | OT::Link | OT::Metadata | OT::Port) {
                    objects_.borrow_mut().push(global.to_owned());
                }
            })
            .register()
    }

    pub fn find_default_metadata_object(
        &self,
        objects: &[PwObject],
    ) -> Result<Option<DefaultMetadata>, pw::Error> {
        let Some(default_metadata_object) = objects.iter().find(|o| is_default_metadata_object(o))
        else {
            return Ok(None);
        };

        let default_metadata = Rc::new(RefCell::new(DefaultMetadata::default()));

        let default_metadata_ = default_metadata.clone();
        let proxy: pw::metadata::Metadata = self.registry.bind(default_metadata_object)?;
        let metadata_listener = proxy
            .add_listener_local()
            .property(move |_, key, _, value| {
                use serde_json::Value as JsonValue;
                if let (Some(key), Some(value)) = (key, value) {
                    let value: JsonValue = serde_json::from_str(value).unwrap();
                    let Some(name_value) = value.get("name") else {
                        log::warn!("Could not get name attribute of default metadata entry: key: {key}, value: {value}");
                        return 0;
                    };
                    let Some(name) = name_value.as_str() else {
                        log::warn!(
                            "Could not interpret name attribute of default metadata entry as string"
                        );
                        return 0;
                    };

                    let name = name.to_owned();
                    let mut proxy = default_metadata_.borrow_mut();

                    match key {
                        "default.configured.audio.sink" => {
                            proxy.default_configured_audio_sink_name = Some(name)
                        }
                        "default.configured.audio.source" => {
                            proxy.default_configured_audio_source_name = Some(name)
                        }
                        "default.audio.sink" => proxy.default_audio_sink_name = Some(name),
                        "default.audio.source" => proxy.default_audio_source_name = Some(name),
                        _ => (),
                    }
                }
                0
            })
            .register();
        let _core_done_listener = register_core_done_listener(self);

        self.core.sync(0)?;
        self.mainloop.run();

        // NOTE: Needed for the line below not to panic
        drop(metadata_listener);
        Ok(Some(Rc::into_inner(default_metadata).unwrap().into_inner()))
    }

    // FIXME: This probably can be done more efficiently, for example by first sorting by audio
    // channel
    pub fn create_links_from_source_to_nodes(
        &self,
        stream: &NodeWithPorts,
        connection_targets: &[NodeWithPorts],
    ) -> Result<(), pw::Error> {
        for stream_port in &stream.ports {
            let stream_channel = stream_port.audio_channel.as_str();
            for target in connection_targets {
                for target_port in &target.ports {
                    let target_channel = target_port.audio_channel.as_str();

                    if stream_channel == "MONO"
                        || target_channel == "MONO"
                        || target_channel == stream_channel
                    {
                        let link = Link::new(stream.id, stream_port.id, target.id, target_port.id);

                        log::debug!("Creating a link: {link:?}");
                        self.create_link(link)?;
                    }
                }
            }
        }

        Ok(())
    }

    pub fn register_stream_proccessing_listener<C>(
        &self,
        stream: &pw::stream::Stream,
        mut consumer: C,
        num_channels: u32,
    ) -> Result<pw::stream::StreamListener<()>, pw::Error>
    where
        C: Consumer<Item = f32> + Send + 'static,
    {
        let mainloop_weak = self.mainloop.downgrade();
        stream
            .add_local_listener()
            .process(move |stream, _: &mut ()| {
                let Some(mut buffer) = stream.dequeue_buffer() else {
                    log::error!("Could not dequeue audio sample buffer!");
                    return;
                };

                let data = &mut buffer.datas_mut()[0];

                if let Some(slice) = data.data() {
                    let slice_len = slice.len();
                    let f32_slice = bytemuck::cast_slice_mut(slice);
                    let read_samples = consumer.pop_slice(f32_slice);
                    if read_samples < f32_slice.len() {
                        f32_slice[read_samples..].fill(0.0);
                    }
                    if read_samples == 0
                        && let Some(loop_handle) = mainloop_weak.upgrade()
                    {
                        log::debug!("Finished playing audio. Stopping pipewire loop...");
                        loop_handle.quit();
                    }

                    let chunk = data.chunk_mut();
                    *chunk.offset_mut() = 0;
                    *chunk.stride_mut() = (size_of::<f32>() as u32 * num_channels) as i32;
                    *chunk.size_mut() = slice_len as u32;
                }
            })
            .register()
    }

    // INFO: Here we need to pass the stream to get its ports since they do not register before the
    // stream becomes active. Pipewire main loop will be stopped after the stream becomes active.
    pub fn get_server_objects(
        &self,
        stream: &pw::stream::Stream,
    ) -> Result<Vec<PwObject>, pw::Error> {
        let objects = Rc::new(RefCell::new(Vec::new()));
        let registry_listener = self.register_registry_listener(&objects);

        let mainloop_weak = self.mainloop.downgrade();
        let _stream_state_listener = stream
            .add_local_listener()
            .state_changed(move |stream, _: &mut (), before, now| {
                log::debug!("Stream state changed from `{before:?}` to `{now:?}`");
                if before == StreamState::Paused && now == StreamState::Streaming {
                    stream.set_active(false).unwrap();
                    log::debug!("Sync complete. Stopping pipewire loop...");
                    mainloop_weak.upgrade().unwrap().quit();
                }
            })
            .register();

        self.sync()?;
        self.mainloop.run();

        // NOTE: Needed for the line below not to panic
        drop(registry_listener);
        Ok(Rc::into_inner(objects).unwrap().into_inner())
    }

    pub fn sync(&self) -> Result<pw::spa::utils::result::AsyncSeq, pw::Error> {
        self.core.sync(0)
    }

    pub fn run_mainloop(&self) {
        self.mainloop.run();
    }
}

fn register_core_done_listener(pw_conn: &PipewireConnection) -> pw::core::Listener {
    let mainloop_weak = pw_conn.mainloop.downgrade();
    pw_conn
        .core
        .add_listener_local()
        .done(move |_, _| {
            if let Some(loop_handle) = mainloop_weak.upgrade() {
                log::debug!("Sync complete. Stopping pipewire loop...");
                loop_handle.quit();
            }
        })
        .register()
}

fn audio_info_from_codec_parameters(
    codec_params: &CodecParameters,
) -> anyhow::Result<AudioInfoRaw> {
    let mut audio_info = AudioInfoRaw::new();

    let format = codec_params
        .sample_format
        .map(sample_format_to_audio_format)
        .unwrap_or(AudioFormat::F32LE);
    audio_info.set_format(format);

    let sample_rate = codec_params
        .sample_rate
        .ok_or(anyhow!("Audio sample rate not found in codec parameters"))?;
    audio_info.set_rate(sample_rate);

    let channels = codec_params
        .channels
        .ok_or(anyhow!("Audio channel data not found in codec parameters"))?;
    let num_channels = channels.bits().count_ones();
    audio_info.set_channels(num_channels);

    let mut position = [0; pw::spa::param::audio::MAX_CHANNELS];
    for (i, chan) in channels.iter().enumerate() {
        position[i] = channels_to_spa_audio_channel(chan);
    }
    audio_info.set_position(position);

    Ok(audio_info)
}
