use std::cell::RefCell;
use std::collections::HashMap;
use std::fmt::Debug;
use std::io;
use std::mem::size_of;
use std::rc::Rc;
use std::str::FromStr;
use std::sync::Arc;
use std::sync::mpsc::{self, Sender};
use std::thread::{self, JoinHandle};

use log::error;
use pipewire::context::ContextRc;
use pipewire::core::{self, CoreRc};
use pipewire::main_loop::MainLoopRc;
use pipewire::metadata::MetadataListener;
use pipewire::properties::{PropertiesBox, properties};
use pipewire::proxy::ProxyT;
use pipewire::registry::{self, GlobalObject, RegistryRc};
use pipewire::spa::pod::serialize::PodSerializer;
use pipewire::spa::pod::{self, Pod};
use pipewire::spa::sys::{SPA_PARAM_EnumFormat, SPA_TYPE_OBJECT_Format};
use pipewire::spa::utils::dict::DictRef;
use pipewire::stream::{StreamFlags, StreamListener, StreamRc, StreamState};
use pipewire::types::ObjectType;
use pipewire::{channel, spa};

use crate::audio_source::AudioSource;
use crate::types::{
    DefaultMetadata, Direction, LinkId, LinkModel, LinkToggleResult, MediaClass, ModelSnapshot,
    NodeId, NodeModel, PortId, PortModel, StreamBuilder, StreamConfig,
};

#[derive(Debug, thiserror::Error)]
pub enum PipewireError {
    #[error("failed to spawn worker thread: {0}")]
    ThreadSpawn(#[from] io::Error),
    #[error("pipewire core error: {0}")]
    Core(#[from] pipewire::Error),
    #[error("stream creation failed: {0}")]
    StreamCreation(String),
    #[error("link creation failed: {0}")]
    LinkCreation(String),
    #[error("link destruction failed")]
    LinkDestruction,
}

pub type PWResult<T> = std::result::Result<T, PipewireError>;

pub type SyncCallback = Box<dyn FnOnce()>;

#[derive(Default)]
pub struct WorkerState {
    snapshot: ModelSnapshot,
    pending_syncs: HashMap<i32, SyncCallback>,
    active_streams: HashMap<NodeId, StreamRc>,
    active_stream_listeners: Vec<StreamListener<()>>,
    active_links: HashMap<LinkId, pipewire::link::Link>,
    global_objects: HashMap<u32, GlobalObject<PropertiesBox>>,
    _default_metadata_proxy: Option<pipewire::metadata::Metadata>,
    _default_metadata_listener: Option<MetadataListener>,
}

type SharedWorkerState = Rc<RefCell<WorkerState>>;

pub type ExecuteFn = Box<dyn FnOnce(&mut WorkerState, &CoreRc) + Send>;

pub enum Command {
    GetSnapshot(Sender<ModelSnapshot>),
    CreateStream {
        config: StreamConfig,
        source: Box<dyn AudioSource>,
        finish_tx: Sender<()>,
        responder: Sender<PWResult<NodeId>>,
    },
    SetStreamState {
        node_id: NodeId,
        playing: bool,
    },
    DestroyStream(NodeId),
    CreateLink {
        out_port: PortId,
        in_port: PortId,
        linger: bool,
        responder: Sender<PWResult<LinkId>>,
    },
    DestroyLink {
        link_id: LinkId,
        responder: Sender<PWResult<()>>,
    },
    Execute(ExecuteFn),
    Shutdown,
}

#[derive(Clone)]
pub struct PipewireClient {
    pub command_tx: pipewire::channel::Sender<Command>,
    pub _worker: Arc<JoinHandle<()>>,
}

impl PipewireClient {
    pub fn new() -> PWResult<Self> {
        let (cmd_tx, cmd_rx) = pipewire::channel::channel::<Command>();
        let (ready_tx, ready_rx) = mpsc::channel();

        let handle = thread::Builder::new()
            .name("pipewire-worker".into())
            .spawn(move || {
                let worker = match Worker::new() {
                    Ok(w) => w,
                    Err(e) => {
                        let _ = ready_tx.send(Err(PipewireError::Core(e)));
                        return;
                    }
                };
                worker.run(cmd_rx, ready_tx);
            })
            .map_err(PipewireError::ThreadSpawn)?;

        ready_rx
            .recv()
            .expect("pipewire worker thread panicked during startup")?;

        Ok(Self {
            command_tx: cmd_tx,
            _worker: Arc::new(handle),
        })
    }

    pub fn send_cmd<T>(
        &self,
        make_cmd: impl FnOnce(Sender<PWResult<T>>) -> Command,
    ) -> PWResult<T> {
        let (tx, rx) = mpsc::channel();
        self.command_tx
            .send(make_cmd(tx))
            .map_err(|_| ())
            .expect("pipewire worker thread is dead");
        rx.recv().expect("pipewire worker terminated unexpectedly")
    }

    pub fn snapshot(&self) -> ModelSnapshot {
        let (tx, rx) = mpsc::channel();
        self.command_tx
            .send(Command::GetSnapshot(tx))
            .map_err(|_| ())
            .expect("pipewire worker thread is dead");
        rx.recv().expect("pipewire worker terminated unexpectedly")
    }

    pub fn link_ports(&self, out_port: PortId, in_port: PortId, linger: bool) -> PWResult<LinkId> {
        self.send_cmd(|responder| Command::CreateLink {
            out_port,
            in_port,
            linger,
            responder,
        })
    }

    pub fn link_nodes(&self, src: NodeId, dst: NodeId, linger: bool) -> PWResult<Vec<LinkId>> {
        let snapshot = self.snapshot();
        let pairs = snapshot.match_ports(src, dst);
        let mut links = Vec::with_capacity(pairs.len());

        for (out_p, in_p) in pairs {
            let link_id = self.link_ports(out_p, in_p, linger)?;
            links.push(link_id);
        }

        Ok(links)
    }

    pub fn destroy_link(&self, link_id: LinkId) -> PWResult<()> {
        self.send_cmd(|responder| Command::DestroyLink { link_id, responder })
    }

    pub fn unlink_nodes(&self, src: NodeId, dst: NodeId) -> PWResult<usize> {
        let snapshot = self.snapshot();
        let link_ids: Vec<LinkId> = snapshot
            .links_between(src, dst)
            .into_iter()
            .map(|l| l.id)
            .collect();

        let link_count = link_ids.len();

        if link_count == 0 {
            return Ok(0);
        }

        for link_id in link_ids {
            self.destroy_link(link_id)?;
        }

        Ok(link_count)
    }

    pub fn toggle_link_nodes(
        &self,
        src: NodeId,
        dst: NodeId,
        linger: bool,
    ) -> PWResult<LinkToggleResult> {
        let snapshot = self.snapshot();
        let existing = snapshot.links_between(src, dst);

        if !existing.is_empty() {
            let removed = self.unlink_nodes(src, dst)?;
            Ok(LinkToggleResult::Unlinked(removed))
        } else {
            let created = self.link_nodes(src, dst, linger)?;
            Ok(LinkToggleResult::Linked(created))
        }
    }

    pub fn stream_builder(&self, source: Box<dyn AudioSource>) -> StreamBuilder<'_> {
        StreamBuilder::new(self, source)
    }

    pub fn execute<F>(&self, f: F)
    where
        F: FnOnce(&mut WorkerState, &CoreRc) + Send + 'static,
    {
        let _ = self.command_tx.send(Command::Execute(Box::new(f)));
    }
}

impl Drop for PipewireClient {
    fn drop(&mut self) {
        if Arc::strong_count(&self._worker) == 1 {
            let _ = self.command_tx.send(Command::Shutdown);
        }
    }
}

#[derive(Clone)]
pub struct Worker {
    mainloop: MainLoopRc,
    core: CoreRc,
    registry: RegistryRc,
    state: SharedWorkerState,
}

impl Worker {
    fn new() -> Result<Self, pipewire::Error> {
        let mainloop = MainLoopRc::new(None)?;
        let props = properties! {
            "pipewire.client.name" => "hijacker",
            "media.category" => "Manager",
        };
        let context = ContextRc::new(&mainloop, Some(props))?;
        let core = context.connect_rc(None)?;
        let registry = core.get_registry_rc()?;
        let state = Rc::new(RefCell::new(WorkerState::default()));

        Ok(Self {
            mainloop,
            core,
            registry,
            state,
        })
    }

    fn run(self, cmd_rx: channel::Receiver<Command>, ready_tx: Sender<PWResult<()>>) {
        let _reg_listener = self.setup_registry_listener();
        let _core_listener = self.setup_core_listener();

        self.sync_barrier_and_respond(ready_tx, ());

        let self_ = self.clone();
        let _cmd_listener = cmd_rx.attach(self.mainloop.loop_(), move |cmd| match cmd {
            Command::GetSnapshot(responder) => {
                let snap = self_.state.borrow().snapshot.clone();
                let _ = responder.send(snap);
            }
            Command::SetStreamState { node_id, playing } => {
                if let Some(stream) = self_.state.borrow().active_streams.get(&node_id) {
                    let _ = stream.set_active(playing);
                }
            }
            Command::DestroyStream(node_id) => {
                self_.state.borrow_mut().active_streams.remove(&node_id);
            }
            Command::CreateLink {
                out_port,
                in_port,
                linger,
                responder,
            } => self_.handle_create_link(out_port, in_port, linger, responder),
            Command::DestroyLink { link_id, responder } => {
                self_.handle_destroy_link(link_id, responder)
            }
            Command::Execute(func) => {
                func(&mut self_.state.borrow_mut(), &self_.core);
            }
            Command::CreateStream {
                config,
                source,
                finish_tx,
                responder,
            } => {
                self_.handle_create_stream(config, source, finish_tx, responder);
            }
            Command::Shutdown => self_.mainloop.quit(),
        });

        self.mainloop.run();
    }

    fn sync_barrier(&self, callback: impl FnOnce() + 'static) -> Result<(), pipewire::Error> {
        let seq = self.core.sync(0)?;
        self.state
            .borrow_mut()
            .pending_syncs
            .insert(seq.raw(), Box::new(callback));
        Ok(())
    }

    /// Performs core.sync(0) and if the sync was successfull, sends value to the caller thread.
    /// If sync was unsuccessfull sends the value anyway.
    fn sync_barrier_and_respond<T: 'static>(&self, responder: Sender<PWResult<T>>, value: T) {
        let responder_ = responder.clone();

        let sync_res = self.sync_barrier(move || {
            let _ = responder_.send(Ok(value));
        });

        if let Err(err) = sync_res {
            let _ = responder.send(Err(PipewireError::Core(err)));
        }
    }

    fn setup_core_listener(&self) -> core::Listener {
        let state = Rc::clone(&self.state);
        self.core
            .add_listener_local()
            .done(move |_id, seq| {
                if let Some(cb) = state.borrow_mut().pending_syncs.remove(&seq.raw()) {
                    cb();
                }
            })
            .register()
    }

    fn setup_registry_listener(&self) -> registry::Listener {
        self.registry
            .add_listener_local()
            .global({
                let self_ = self.clone();
                move |obj| self_.on_global_added(obj)
            })
            .global_remove({
                let self_ = self.clone();
                move |id| self_.on_global_removed(id)
            })
            .register()
    }

    fn on_global_added(&self, obj: &GlobalObject<&DictRef>) {
        let mut state = self.state.borrow_mut();
        state.global_objects.insert(obj.id, obj.to_owned());

        match obj.type_ {
            ObjectType::Node => {
                let props = obj.props.as_ref();
                let node = NodeModel {
                    id: NodeId(obj.id),
                    name: props
                        .and_then(|p| p.get("node.name"))
                        .unwrap_or_default()
                        .into(),
                    description: props
                        .and_then(|p| p.get("node.description"))
                        .map(Into::into),
                    media_class: props
                        .and_then(|p| p.get("media.class"))
                        .map(MediaClass::from),
                    app_name: props
                        .and_then(|p| p.get("application.name"))
                        .map(Into::into),
                    pid: props
                        .and_then(|p| p.get("application.process.id"))
                        .and_then(|p| p.parse().ok()),
                };
                state.snapshot.nodes.insert(node.id, node);
            }
            ObjectType::Port => {
                let props = obj.props.as_ref();
                let Some(node_id) = props
                    .and_then(|p| p.get("node.id"))
                    .and_then(|id| id.parse().ok())
                else {
                    return;
                };

                let direction = props
                    .and_then(|p| p.get("port.direction"))
                    .and_then(|d| Direction::from_str(d).ok())
                    .unwrap_or(Direction::Output);

                let channel = props
                    .and_then(|p| p.get("audio.channel"))
                    .unwrap_or("MONO")
                    .to_string();

                let port_id = PortId(obj.id);
                let parent_node_id = NodeId(node_id);

                let port = PortModel {
                    id: port_id,
                    node_id: parent_node_id,
                    direction,
                    channel,
                };

                state.snapshot.ports.insert(port_id, port);
                state
                    .snapshot
                    .node_ports
                    .entry(parent_node_id)
                    .or_default()
                    .push(port_id);
            }
            ObjectType::Link => {
                let props = obj.props.as_ref();
                let Some(out_n) = props
                    .and_then(|p| p.get("link.output.node"))
                    .and_then(|id| id.parse().ok())
                else {
                    return;
                };
                let Some(out_p) = props
                    .and_then(|p| p.get("link.output.port"))
                    .and_then(|id| id.parse().ok())
                else {
                    return;
                };
                let Some(in_n) = props
                    .and_then(|p| p.get("link.input.node"))
                    .and_then(|id| id.parse().ok())
                else {
                    return;
                };
                let Some(in_p) = props
                    .and_then(|p| p.get("link.input.port"))
                    .and_then(|id| id.parse().ok())
                else {
                    return;
                };

                let link = LinkModel {
                    id: LinkId(obj.id),
                    output_node: NodeId(out_n),
                    output_port: PortId(out_p),
                    input_node: NodeId(in_n),
                    input_port: PortId(in_p),
                };
                state.snapshot.links.insert(link.id, link);
            }
            ObjectType::Metadata => {
                let is_default =
                    obj.props.as_ref().and_then(|p| p.get("metadata.name")) == Some("default");

                if !is_default {
                    return;
                }

                let metadata_proxy: pipewire::metadata::Metadata = match self.registry.bind(obj) {
                    Ok(proxy) => proxy,
                    Err(e) => {
                        error!("failed to bind default metadata proxy: {e}");
                        return;
                    }
                };

                let state_ = Rc::clone(&self.state);
                let listener = metadata_proxy
                    .add_listener_local()
                    .property(move |_, key, _, value| {
                        let Some((key, value)) = key.zip(value) else {
                            return 0;
                        };

                        let entry_name = parse_named_entry(value);

                        let mut st = state_.borrow_mut();
                        match key {
                            "default.configured.audio.sink" => {
                                st.snapshot.metadata.default_configured_audio_sink = entry_name;
                            }
                            "default.configured.audio.source" => {
                                st.snapshot.metadata.default_configured_audio_source = entry_name;
                            }
                            "default.audio.sink" => {
                                st.snapshot.metadata.default_audio_sink = entry_name;
                            }
                            "default.audio.source" => {
                                st.snapshot.metadata.default_audio_source = entry_name;
                            }
                            _ => {}
                        }

                        0
                    })
                    .register();

                state._default_metadata_proxy = Some(metadata_proxy);
                state._default_metadata_listener = Some(listener);
            }
            _ => {}
        }
    }

    fn on_global_removed(&self, id: u32) {
        let mut state = self.state.borrow_mut();
        state.global_objects.remove(&id);

        let node_id = NodeId(id);
        let port_id = PortId(id);
        let link_id = LinkId(id);

        state.snapshot.nodes.remove(&node_id);
        state.snapshot.links.remove(&link_id);
        state.active_links.remove(&link_id);
        state.active_streams.remove(&node_id);

        if let Some(port) = state.snapshot.ports.remove(&port_id)
            && let Some(ports) = state.snapshot.node_ports.get_mut(&port.node_id)
        {
            ports.retain(|&p| p != port_id);
        }
        state.snapshot.node_ports.remove(&node_id);

        if state
            ._default_metadata_proxy
            .as_ref()
            .is_some_and(|p| p.upcast_ref().id() == id)
        {
            state._default_metadata_proxy = None;
            state._default_metadata_listener = None;
            state.snapshot.metadata = DefaultMetadata::default();
        }
    }

    fn handle_create_link(
        &self,
        out_port: PortId,
        in_port: PortId,
        linger: bool,
        responder: Sender<PWResult<LinkId>>,
    ) {
        let props = properties! {
            "link.output.port" => out_port.0.to_string(),
            "link.input.port" => in_port.0.to_string(),
            "object.linger" => if linger { "1" } else { "0" },
        };

        let link_proxy_res = self
            .core
            .create_object::<pipewire::link::Link>("link-factory", &props)
            .map_err(|e| PipewireError::LinkCreation(e.to_string()));
        let link_proxy = match link_proxy_res {
            Ok(v) => v,
            Err(err) => {
                let _ = responder.send(Err(err));
                return;
            }
        };

        let link_id = LinkId(link_proxy.upcast_ref().id());
        self.state
            .borrow_mut()
            .active_links
            .insert(link_id, link_proxy);

        self.sync_barrier_and_respond(responder, link_id);
    }

    fn handle_destroy_link(&self, link_id: LinkId, responder: Sender<PWResult<()>>) {
        self.state.borrow_mut().active_links.remove(&link_id);

        if self
            .registry
            .destroy_global(link_id.0)
            .into_result()
            .is_err()
        {
            let _ = responder.send(Err(PipewireError::LinkDestruction));
        }

        self.sync_barrier_and_respond(responder, ());
    }

    fn handle_create_stream(
        &self,
        config: StreamConfig,
        mut audio_source: Box<dyn AudioSource>,
        stream_finished_tx: Sender<()>,
        responder: Sender<PWResult<NodeId>>,
    ) {
        let num_channels = audio_source.audio_info().channels();
        let stream_props = properties! {
            *pipewire::keys::MEDIA_TYPE => "Audio",
            *pipewire::keys::MEDIA_CATEGORY => "Playback",
            *pipewire::keys::MEDIA_ROLE => "Music",
            *pipewire::keys::AUDIO_CHANNELS => num_channels.to_string(),
        };

        let stream = match StreamRc::new(self.core.clone(), &config.name, stream_props) {
            Ok(s) => s,
            Err(e) => {
                let _ = responder.send(Err(PipewireError::StreamCreation(e.to_string())));
                return;
            }
        };

        let mut flags = StreamFlags::MAP_BUFFERS;
        if config.autoconnect {
            flags |= StreamFlags::AUTOCONNECT;
        }

        let obj = pod::Object {
            type_: SPA_TYPE_OBJECT_Format,
            id: SPA_PARAM_EnumFormat,
            properties: (*audio_source.audio_info()).into(),
        };

        let serializer =
            match PodSerializer::serialize(io::Cursor::new(Vec::new()), &pod::Value::Object(obj)) {
                Ok(s) => s,
                Err(e) => {
                    let _ = responder.send(Err(PipewireError::StreamCreation(e.to_string())));
                    return;
                }
            };

        let values = serializer.0.into_inner();
        let Some(pod_buf) = Pod::from_bytes(&values) else {
            let _ = responder.send(Err(PipewireError::StreamCreation(
                "failed to parse POD buffer".into(),
            )));
            return;
        };
        let mut params = [pod_buf];

        if let Err(e) = stream.connect(spa::utils::Direction::Output, None, flags, &mut params) {
            let _ = responder.send(Err(PipewireError::StreamCreation(e.to_string())));
            return;
        }

        let responder_ = responder.clone();
        let stream_ = stream.clone();
        let self_ = self.clone();

        let listener = match stream
            .add_local_listener()
            .state_changed(move |stream, _, old, new| match new {
                StreamState::Streaming if old == StreamState::Paused => {
                    let node_id = NodeId(stream.node_id());
                    self_
                        .state
                        .borrow_mut()
                        .active_streams
                        .insert(node_id, stream_.clone());

                    self_.sync_barrier_and_respond(responder_.clone(), node_id);
                }
                StreamState::Error(err_msg) => {
                    let _ =
                        responder_.send(Err(PipewireError::StreamCreation(err_msg.to_string())));
                }
                StreamState::Unconnected => {
                    let _ = responder_.send(Err(PipewireError::StreamCreation(
                        "stream disconnected unexpectedly during setup".into(),
                    )));
                }
                _ => {}
            })
            .process(move |steam, _| {
                let Some(mut buf) = steam.dequeue_buffer() else {
                    return;
                };
                let data = &mut buf.datas_mut()[0];
                let Some(samples_slice) = data.data() else {
                    return;
                };
                let samples_slice_len = samples_slice.len();

                let (prefix, f32_samples_slice, suffix) =
                    unsafe { samples_slice.align_to_mut::<f32>() };
                if !prefix.is_empty() || !suffix.is_empty() {
                    error!("slice of u8 is not properly aligned or sized for f32");
                    return;
                }

                let (filled_samples_slice, empty_samples_slice) =
                    audio_source.consumer().pop_partial_slice(f32_samples_slice);

                if !empty_samples_slice.is_empty() {
                    empty_samples_slice.fill(0.0);
                }

                if filled_samples_slice.is_empty() {
                    let _ = stream_finished_tx.send(());
                }

                let chunk = data.chunk_mut();
                *chunk.offset_mut() = 0;
                *chunk.stride_mut() = (size_of::<f32>() as u32 * num_channels) as i32;
                *chunk.size_mut() = samples_slice_len as u32;
            })
            .register()
        {
            Ok(l) => l,
            Err(err) => {
                let _ = responder.send(Err(PipewireError::StreamCreation(format!(
                    "failed to register stream listener: {err}"
                ))));
                return;
            }
        };

        self.state
            .borrow_mut()
            .active_stream_listeners
            .push(listener);

        let _ = self.core.sync(0);
    }
}

fn parse_named_entry(object: &str) -> Option<String> {
    let stripped_obj = object.trim().strip_circumfix("{", "}")?.trim();
    let stripped_obj = stripped_obj.strip_suffix(',').unwrap_or(stripped_obj);
    let mut prop_value = stripped_obj.split(':');
    let prop = prop_value.next()?.trim().strip_circumfix('"', '"')?;
    let entry = prop_value.next()?.trim().strip_circumfix('"', '"')?;
    if prop == "name" {
        Some(entry.into())
    } else {
        None
    }
}

#[cfg(test)]
mod test {
    use super::*;

    #[test]
    fn parse_default_metadata_prop() {
        let json = r#"
            {
                "name": "something",
            }
        "#;

        assert_eq!(parse_named_entry(json), Some("something".to_string()));
    }
}
