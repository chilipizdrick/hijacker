use std::{cell::RefCell, collections::HashMap, rc::Rc, sync::mpsc, thread};

use pipewire::{
    context::Context,
    core::Core,
    link,
    main_loop::MainLoop,
    properties::properties,
    registry::{GlobalObject, Registry},
    spa::utils::dict::DictRef,
    types::ObjectType,
};

use crate::{Error, Result};

#[derive(Debug)]
enum PwRequest {
    Quit,
    Ports,
    Nodes,
    CreateLink(LinkInfo),
    RemoveLink(Link),
}

#[derive(Debug)]
enum PwResponse {
    Quit,
    Ports(Vec<Port>),
    Nodes(Vec<Node>),
    CreateLink(Link),
    RemoveLink,
}

pub struct Client {
    pw_sender: pipewire::channel::Sender<PwRequest>,
    main_receiver: mpsc::Receiver<PwResponse>,
}

impl Client {
    pub fn new() -> Self {
        let (main_sender, main_receiver) = mpsc::channel();
        let (pw_sender, pw_receiver) = pipewire::channel::channel();

        thread::spawn(move || {
            let thread_worker = ThreadWorker::try_new(main_sender, pw_receiver).unwrap();
            thread_worker.run();
        });

        Self {
            pw_sender,
            main_receiver,
        }
    }

    pub fn quit(&self) -> Result<()> {
        match self.generic_requset(PwRequest::Quit)? {
            PwResponse::Quit => Ok(()),
            _ => Err(Error::UnexpectedPwResponse),
        }
    }

    pub fn application_nodes(&self) -> Result<Vec<Node>> {
        match self.generic_requset(PwRequest::Nodes)? {
            PwResponse::Nodes(nodes) => Ok(nodes),
            _ => Err(Error::UnexpectedPwResponse),
        }
    }

    pub fn ports(&self) -> Result<Vec<Port>> {
        match self.generic_requset(PwRequest::Ports)? {
            PwResponse::Ports(ports) => Ok(ports),
            _ => Err(Error::UnexpectedPwResponse),
        }
    }

    pub fn create_link(&self, link_info: LinkInfo) -> Result<Link> {
        match self.generic_requset(PwRequest::CreateLink(link_info))? {
            PwResponse::CreateLink(link) => Ok(link),
            _ => Err(Error::UnexpectedPwResponse),
        }
    }

    pub fn remove_link(&self, link: Link) -> Result<()> {
        match self.generic_requset(PwRequest::RemoveLink(link))? {
            PwResponse::RemoveLink => Ok(()),
            _ => Err(Error::UnexpectedPwResponse),
        }
    }

    fn generic_requset(&self, request: PwRequest) -> Result<PwResponse> {
        self.pw_sender.send(request).map_err(|_| Error::Send)?;
        self.main_receiver.recv().map_err(Error::Recv)
    }
}

struct ThreadWorker {
    main_sender: mpsc::Sender<PwResponse>,
    pw_reciever: pipewire::channel::Receiver<PwRequest>,
    main_loop: MainLoop,
    _context: Context,
    core: Core,
    registry: Registry,
    state: Rc<RefCell<ClientState>>,
}

#[derive(Default, Debug)]
struct ClientState {
    nodes: HashMap<u32, Node>,
    ports: HashMap<u32, Port>,
    links: IncrementalMap<link::Link>,
}

impl ThreadWorker {
    fn try_new(
        main_sender: mpsc::Sender<PwResponse>,
        pw_reciever: pipewire::channel::Receiver<PwRequest>,
    ) -> Result<Self> {
        let main_loop = MainLoop::new(None).map_err(Error::Pipewire)?;
        let context = Context::new(&main_loop).map_err(Error::Pipewire)?;
        let core = context.connect(None).map_err(Error::Pipewire)?;
        let registry = core.get_registry().map_err(Error::Pipewire)?;

        Ok(Self {
            main_sender,
            pw_reciever,
            main_loop,
            _context: context,
            core,
            registry,
            state: Rc::new(RefCell::new(ClientState::default())),
        })
    }

    fn run(self) {
        let global_state = Rc::clone(&self.state);
        let global_remove_state = Rc::clone(&self.state);

        let _listener = self
            .registry
            .add_listener_local()
            .global(move |object| match object.type_ {
                ObjectType::Node => Self::handle_node(object, &global_state),
                ObjectType::Port => Self::handle_port(object, &global_state),
                _ => {}
            })
            .global_remove(move |object_id| {
                global_remove_state.borrow_mut().nodes.remove(&object_id);
                global_remove_state.borrow_mut().ports.remove(&object_id);
            })
            .register();

        let _reciever = self.pw_reciever.attach(self.main_loop.loop_(), {
            let main_loop = self.main_loop.clone();
            let state = Rc::clone(&self.state);
            move |request: PwRequest| match request {
                PwRequest::Quit => {
                    main_loop.quit();
                    self.main_sender.send(PwResponse::Quit).unwrap();
                }
                PwRequest::Ports => {
                    let ports = state.borrow().ports.values().cloned().collect();
                    self.main_sender.send(PwResponse::Ports(ports)).unwrap();
                }
                PwRequest::Nodes => {
                    let nodes = state.borrow().nodes.values().cloned().collect();
                    self.main_sender.send(PwResponse::Nodes(nodes)).unwrap();
                }
                PwRequest::CreateLink(link_info) => {
                    let link = Self::create_link(&self.core, &link_info).unwrap();
                    let link_id = state.borrow_mut().links.insert(link);
                    self.main_sender
                        .send(PwResponse::CreateLink(Link::new(link_id)))
                        .unwrap();
                }
                PwRequest::RemoveLink(link) => {
                    let links = &mut state.borrow_mut().links;
                    let link = links.remove(link.internal_link_id).unwrap();
                    Self::remove_link(&self.core, link).unwrap();
                    self.main_sender.send(PwResponse::RemoveLink).unwrap();
                }
            }
        });

        self.main_loop.run();
    }

    fn handle_node(object: &GlobalObject<&DictRef>, state: &Rc<RefCell<ClientState>>) {
        let props = object.props.as_ref().unwrap();
        let media_class = props.get("media.class");
        let media_type = props.get("media.type");
        if media_type == Some("Audio") && media_class.is_some_and(|s| s.starts_with("Stream/")) {
            let node = Node::from_global_obj(object);
            state.borrow_mut().nodes.insert(node.id(), node);
        }
    }

    fn handle_port(object: &GlobalObject<&DictRef>, state: &Rc<RefCell<ClientState>>) {
        let port = Port::from_global_obj(object);
        state.borrow_mut().ports.insert(port.id(), port);
    }

    fn create_link(core: &Core, link: &LinkInfo) -> Result<link::Link> {
        core.create_object::<link::Link>(
            "link-factory",
            &properties! {
                "link.output.node" => link.output_node_id.to_string(),
                "link.output.port" => link.output_port_id.to_string(),
                "link.input.node" => link.input_node_id.to_string(),
                "link.input.port" => link.input_port_id.to_string(),
                "object.linger" => "1"
            },
        )
        .map_err(Error::Pipewire)
    }

    fn remove_link(core: &Core, link: link::Link) -> Result<()> {
        core.destroy_object(link).map_err(Error::Pipewire)?;
        Ok(())
    }
}

#[derive(Clone, Debug)]
pub struct Node {
    id: u32,
    application_name: Option<String>,
    node_name: String,
}

impl Node {
    pub fn application_name(&self) -> Option<&String> {
        self.application_name.as_ref()
    }

    pub fn id(&self) -> u32 {
        self.id
    }

    pub fn node_name(&self) -> &String {
        &self.node_name
    }

    fn from_global_obj(object: &GlobalObject<&DictRef>) -> Self {
        let props = object.props.as_ref().unwrap();
        Self {
            id: object.id,
            application_name: props.get("application.name").map(|s| s.to_string()),
            node_name: props.get("node.name").map(|s| s.to_string()).unwrap(),
        }
    }
}

#[derive(Clone, Debug)]
pub struct Port {
    id: u32,
    node_id: u32,
    port_id: u32,
    direction: PortDirection,
}

impl Port {
    pub fn id(&self) -> u32 {
        self.id
    }

    pub fn port_id(&self) -> u32 {
        self.port_id
    }

    pub fn direction(&self) -> PortDirection {
        self.direction
    }

    pub fn node_id(&self) -> u32 {
        self.node_id
    }

    fn from_global_obj(object: &GlobalObject<&DictRef>) -> Self {
        let props = object.props.as_ref().unwrap();

        let direction = match props.get("port.direction").unwrap() {
            "in" => PortDirection::In,
            "out" => PortDirection::Out,
            _ => panic!("ERROR: Unexpected port.direction"),
        };

        Self {
            id: object.id,
            node_id: props
                .get("node.id")
                .map(|s| s.parse::<u32>().unwrap())
                .unwrap(),
            port_id: props
                .get("port.id")
                .map(|s| s.parse::<u32>().unwrap())
                .unwrap(),
            direction,
        }
    }
}

#[derive(Copy, Clone, Debug)]
pub enum PortDirection {
    Out,
    In,
}

#[derive(Clone, Copy, Debug)]
pub struct LinkInfo {
    output_node_id: u32,
    output_port_id: u32,
    input_node_id: u32,
    input_port_id: u32,
}

impl LinkInfo {
    pub fn new(
        output_node_id: u32,
        output_port_id: u32,
        input_node_id: u32,
        input_port_id: u32,
    ) -> Self {
        Self {
            output_node_id,
            output_port_id,
            input_node_id,
            input_port_id,
        }
    }
}

#[derive(Debug)]
pub struct Link {
    internal_link_id: u32,
}

impl Link {
    fn new(id: u32) -> Self {
        Self {
            internal_link_id: id,
        }
    }
}

#[derive(Debug)]
struct IncrementalMap<V> {
    map: HashMap<u32, V>,
    key: u32,
}

impl<V> IncrementalMap<V> {
    fn new() -> Self {
        Self {
            map: HashMap::new(),
            key: 0,
        }
    }

    fn insert(&mut self, value: V) -> u32 {
        self.key += 1;
        self.map.insert(self.key, value);
        self.key
    }

    fn remove(&mut self, key: u32) -> Option<V> {
        self.map.remove(&key)
    }
}

impl<V> Default for IncrementalMap<V> {
    fn default() -> Self {
        Self::new()
    }
}
