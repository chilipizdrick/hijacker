use std::cell::RefCell;
use std::collections::HashMap;
use std::fmt::Debug;
use std::io;
use std::mem::size_of;
use std::rc::Rc;
use std::str::FromStr;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Sender};
use std::thread::{self, JoinHandle};
use std::time::Duration;

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
use serde::Deserialize;

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
    active_stream_finished: Vec<(Arc<AtomicBool>, Option<Sender<()>>)>,
    active_links: HashMap<LinkId, pipewire::link::Link>,
    global_objects: HashMap<u32, GlobalObject<PropertiesBox>>,
    _default_metadata_proxy: Option<pipewire::metadata::Metadata>,
    _default_metadata_listener: Option<MetadataListener>,
}

type SharedWorkerState = Rc<RefCell<WorkerState>>;

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
    Execute(Box<dyn FnOnce(&mut WorkerState, &CoreRc) + Send>),
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
            "pipewire.acess" => "all",
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

        if let Err(e) = self.sync_barrier({
            let ready_tx = ready_tx.clone();
            move || {
                let _ = ready_tx.send(Ok(()));
            }
        }) {
            let _ = ready_tx.send(Err(PipewireError::Core(e)));
            return;
        }

        let mainloop = self.mainloop.clone();
        let state = Rc::clone(&self.state);
        let core = self.core.clone();

        let timer_source = self.mainloop.loop_().add_timer({
            let state = Rc::clone(&state);
            move |_| {
                let mut st = state.borrow_mut();
                for (is_done, finish_tx) in &mut st.active_stream_finished {
                    if is_done.load(Ordering::Acquire)
                        && let Some(tx) = finish_tx.take()
                    {
                        let _ = tx.send(());
                    }
                }
                st.active_stream_finished.retain(|(_, tx)| tx.is_some());
            }
        });
        timer_source.update_timer(
            Some(Duration::from_millis(20)),
            Some(Duration::from_millis(20)),
        );

        let _cmd_listener = cmd_rx.attach(self.mainloop.loop_(), move |cmd| match cmd {
            Command::GetSnapshot(responder) => {
                let snap = state.borrow().snapshot.clone();
                let _ = responder.send(snap);
            }
            Command::SetStreamState { node_id, playing } => {
                if let Some(stream) = state.borrow().active_streams.get(&node_id) {
                    let _ = stream.set_active(playing);
                }
            }
            Command::DestroyStream(node_id) => {
                state.borrow_mut().active_streams.remove(&node_id);
            }
            Command::CreateLink {
                out_port,
                in_port,
                linger,
                responder,
            } => {
                let link_res = Self::handle_create_link(&core, &state, out_port, in_port, linger);
                let _ = responder.send(link_res);
            }
            Command::DestroyLink { link_id, responder } => {
                state.borrow_mut().active_links.remove(&link_id);

                if let Err(_) = self.registry.destroy_global(link_id.0).into_result() {
                    let _ = responder.send(Err(PipewireError::LinkDestruction));
                }

                if let Ok(seq) = core.sync(0) {
                    self.state.borrow_mut().pending_syncs.insert(
                        seq.raw(),
                        Box::new(move || {
                            let _ = responder.send(Ok(()));
                        }),
                    );
                } else {
                    let _ = responder.send(Ok(()));
                }
            }
            Command::Execute(func) => {
                func(&mut state.borrow_mut(), &core);
            }
            Command::CreateStream {
                config,
                source,
                finish_tx,
                responder,
            } => {
                Self::handle_create_stream(&core, &state, config, source, finish_tx, responder);
            }
            Command::Shutdown => mainloop.quit(),
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
        let state = Rc::clone(&self.state);
        let registry = self.registry.clone();

        self.registry
            .add_listener_local()
            .global({
                let state = Rc::clone(&state);
                let registry = registry.clone();
                move |obj| Self::on_global_added(&registry, &state, obj)
            })
            .global_remove({
                let state = Rc::clone(&state);
                move |id| Self::on_global_removed(&state, id)
            })
            .register()
    }

    fn on_global_added(
        registry: &RegistryRc,
        state: &SharedWorkerState,
        obj: &GlobalObject<&DictRef>,
    ) {
        let mut st = state.borrow_mut();
        st.global_objects.insert(obj.id, obj.to_owned());

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
                st.snapshot.nodes.insert(node.id, node);
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

                st.snapshot.ports.insert(port_id, port);
                st.snapshot
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
                st.snapshot.links.insert(link.id, link);
            }
            ObjectType::Metadata => {
                let is_default =
                    obj.props.as_ref().and_then(|p| p.get("metadata.name")) == Some("default");

                if !is_default {
                    return;
                }

                let metadata_proxy: pipewire::metadata::Metadata = match registry.bind(obj) {
                    Ok(proxy) => proxy,
                    Err(e) => {
                        error!("failed to bind default metadata proxy: {e}");
                        return;
                    }
                };

                let state_ref = Rc::clone(state);
                let listener = metadata_proxy
                    .add_listener_local()
                    .property(move |_, key, _, value| {
                        let Some((key, value)) = key.zip(value) else {
                            return 0;
                        };

                        #[derive(Deserialize)]
                        struct NamedEntry {
                            name: String,
                        }

                        let entry_name = serde_json::from_str::<NamedEntry>(value)
                            .ok()
                            .map(|e| e.name);

                        let mut st = state_ref.borrow_mut();
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

                st._default_metadata_proxy = Some(metadata_proxy);
                st._default_metadata_listener = Some(listener);
            }
            _ => {}
        }
    }

    fn on_global_removed(state: &SharedWorkerState, id: u32) {
        let mut st = state.borrow_mut();
        st.global_objects.remove(&id);

        let node_id = NodeId(id);
        let port_id = PortId(id);
        let link_id = LinkId(id);

        st.snapshot.nodes.remove(&node_id);
        st.snapshot.links.remove(&link_id);
        st.active_links.remove(&link_id);
        st.active_streams.remove(&node_id);

        if let Some(port) = st.snapshot.ports.remove(&port_id)
            && let Some(ports) = st.snapshot.node_ports.get_mut(&port.node_id)
        {
            ports.retain(|&p| p != port_id);
        }
        st.snapshot.node_ports.remove(&node_id);

        if st
            ._default_metadata_proxy
            .as_ref()
            .is_some_and(|p| p.upcast_ref().id() == id)
        {
            st._default_metadata_proxy = None;
            st._default_metadata_listener = None;
            st.snapshot.metadata = DefaultMetadata::default();
        }
    }

    fn handle_create_link(
        core: &CoreRc,
        state: &SharedWorkerState,
        out_port: PortId,
        in_port: PortId,
        linger: bool,
    ) -> PWResult<LinkId> {
        let props = properties! {
            "link.output.port" => out_port.0.to_string(),
            "link.input.port" => in_port.0.to_string(),
            "object.linger" => if linger { "1" } else { "0" },
        };

        let link_proxy = core
            .create_object::<pipewire::link::Link>("link-factory", &props)
            .map_err(|e| PipewireError::LinkCreation(e.to_string()))?;

        let link_id = LinkId(link_proxy.upcast_ref().id());
        state.borrow_mut().active_links.insert(link_id, link_proxy);
        Ok(link_id)
    }

    fn handle_create_stream(
        core: &CoreRc,
        state: &SharedWorkerState,
        config: StreamConfig,
        mut audio_source: Box<dyn AudioSource>,
        finish_tx: Sender<()>,
        responder: Sender<PWResult<NodeId>>,
    ) {
        let num_channels = audio_source.audio_info().channels();
        let stream_props = properties! {
            *pipewire::keys::MEDIA_TYPE => "Audio",
            *pipewire::keys::MEDIA_CATEGORY => "Playback",
            *pipewire::keys::MEDIA_ROLE => "Music",
            *pipewire::keys::AUDIO_CHANNELS => num_channels.to_string(),
        };

        let stream = match StreamRc::new(core.clone(), &config.name, stream_props) {
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
            properties: audio_source.audio_info().clone().into(),
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

        let is_done = Arc::new(AtomicBool::new(false));
        state
            .borrow_mut()
            .active_stream_finished
            .push((Arc::clone(&is_done), Some(finish_tx)));

        let responder_cell = Rc::new(RefCell::new(Some(responder)));
        let state_clone = Rc::clone(state);
        let stream_clone = stream.clone();
        let core_clone = core.clone();

        let listener = match stream
            .add_local_listener()
            .state_changed(move |s, _, old, new| match new {
                StreamState::Streaming if old == StreamState::Paused => {
                    let node_id = NodeId(s.node_id());
                    state_clone
                        .borrow_mut()
                        .active_streams
                        .insert(node_id, stream_clone.clone());

                    let state_inner = Rc::clone(&state_clone);
                    let resp_cell = Rc::clone(&responder_cell);
                    if let Ok(seq) = core_clone.sync(0) {
                        state_inner.borrow_mut().pending_syncs.insert(
                            seq.raw(),
                            Box::new(move || {
                                if let Some(resp) = resp_cell.borrow_mut().take() {
                                    let _ = resp.send(Ok(node_id));
                                }
                            }),
                        );
                    } else if let Some(resp) = resp_cell.borrow_mut().take() {
                        let _ = resp.send(Ok(node_id));
                    }
                }
                StreamState::Error(err_msg) => {
                    if let Some(resp) = responder_cell.borrow_mut().take() {
                        let _ = resp.send(Err(PipewireError::StreamCreation(err_msg.to_string())));
                    }
                }
                StreamState::Unconnected => {
                    if let Some(resp) = responder_cell.borrow_mut().take() {
                        let _ = resp.send(Err(PipewireError::StreamCreation(
                            "stream disconnected unexpectedly during setup".into(),
                        )));
                    }
                }
                _ => {}
            })
            .process(move |s, _| {
                let Some(mut buf) = s.dequeue_buffer() else {
                    return;
                };
                let data = &mut buf.datas_mut()[0];
                let Some(slice) = data.data() else {
                    return;
                };

                let slice_len = slice.len();
                let f32_slice = bytemuck::cast_slice_mut(slice);
                let (filled, empty) = audio_source.consumer().pop_partial_slice(f32_slice);

                if !empty.is_empty() {
                    empty.fill(0.0);
                }

                if filled.is_empty() {
                    is_done.store(true, Ordering::Release);
                }

                let chunk = data.chunk_mut();
                *chunk.offset_mut() = 0;
                *chunk.stride_mut() = (size_of::<f32>() as u32 * num_channels) as i32;
                *chunk.size_mut() = slice_len as u32;
            })
            .register()
        {
            Ok(l) => l,
            Err(e) => {
                error!("failed to register stream listener: {e}");
                return;
            }
        };

        state.borrow_mut().active_stream_listeners.push(listener);
        let _ = core.sync(0);
    }
}
