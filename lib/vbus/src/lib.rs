use std::fmt;
use std::pin::Pin;
use std::task::{Context, Poll};
use thiserror::Error;

pub use wasmer_vfs::FileDescriptor;
pub use wasmer_vfs::StdioMode;
use wasmer_vfs::VirtualFile;

pub type Result<T> = std::result::Result<T, VirtualBusError>;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
#[repr(transparent)]
pub struct CallDescriptor(u32);

impl CallDescriptor {
    pub fn raw(&self) -> u32 {
        self.0
    }
}

impl From<u32> for CallDescriptor {
    fn from(a: u32) -> Self {
        Self(a)
    }
}

pub trait VirtualBus<T>: fmt::Debug + Send + Sync + 'static
where T: SpawnEnvironmentIntrinsics,
      T: std::fmt::Debug + Send + Sync + 'static
{
    /// Spawns a brand new process
    fn spawn_new(&self) -> NewSpawnBuilder<T> {
        NewSpawnBuilder::new(Box::new(UnsupportedVirtualBusSpawner::default()))
    }

    /// Starts a new WAPM sub process
    fn spawn_existing(&self, env: T) -> SpawnOptions<T> {
        SpawnOptions::new(
            Box::new(UnsupportedVirtualBusSpawner::default()),
            SpawnType::Existing { env }
        )
    }

    /// Creates a listener thats used to receive BUS commands
    fn listen<'a>(&'a self) -> Result<&'a dyn VirtualBusListener> {
        Err(VirtualBusError::Unsupported)
    }
}

pub struct NewSpawnBuilder<T>
{
    spawner: Box<dyn VirtualBusSpawner<T>>,
    reuse: bool,
    chroot: bool,
    args: Vec<String>,
    preopen: Vec<String>,
    stdin_mode: StdioMode,
    stdout_mode: StdioMode,
    stderr_mode: StdioMode,
    working_dir: Option<String>,
    remote_instance: Option<String>,
    access_token: Option<String>,
}

impl<T> NewSpawnBuilder<T>
{
    pub fn new(spawner: Box<dyn VirtualBusSpawner<T>>) -> Self {
        Self {
            spawner,
            reuse: false,
            chroot: false,
            args: Vec::new(),
            preopen: Vec::new(),
            stdin_mode: StdioMode::Null,
            stdout_mode: StdioMode::Null,
            stderr_mode: StdioMode::Null,
            working_dir: None,
            remote_instance: None,
            access_token: None,
        }
    }

    pub fn reuse(mut self, reuse: bool) -> Self {
        self.reuse = reuse;
        self
    }

    pub fn chroot(mut self, chroot: bool) -> Self {
        self.chroot = chroot;
        self
    }

    pub fn args(mut self, args: Vec<String>) -> Self {
        self.args = args;
        self
    }

    pub fn preopen(mut self, preopen: Vec<String>) -> Self {
        self.preopen = preopen;
        self
    }

    pub fn working_dir(mut self, working_dir: String) -> Self {
        self.working_dir = Some(working_dir);
        self
    }

    pub fn stdin_mode(mut self, stdin_mode: StdioMode) -> Self {
        self.stdin_mode = stdin_mode;
        self
    }

    pub fn stdout_mode(mut self, stdout_mode: StdioMode) -> Self {
        self.stdout_mode = stdout_mode;
        self
    }

    pub fn stderr_mode(mut self, stderr_mode: StdioMode) -> Self {
        self.stderr_mode = stderr_mode;
        self
    }

    pub fn remote_instance(mut self, remote_instance: String) -> Self {
        self.remote_instance = Some(remote_instance);
        self
    }

    pub fn access_token(mut self, access_token: String) -> Self {
        self.access_token = Some(access_token);
        self
    }

    pub fn build(self) -> SpawnOptions<T>
    where T: SpawnEnvironmentIntrinsics,
          T: std::fmt::Debug + Send + Sync + 'static
    {
        let mut ret = SpawnOptions::new(self.spawner, SpawnType::New {
            chroot: self.chroot,
            args: self.args,
            preopen: self.preopen,
            stdin_mode: self.stdin_mode,
            stdout_mode: self.stdout_mode,
            stderr_mode: self.stderr_mode,
            working_dir: self.working_dir
        });
        ret.conf.reuse = self.reuse;
        ret.conf.access_token = self.access_token;
        ret.conf.remote_instance = self.remote_instance;
        ret
    }
}

pub trait VirtualBusSpawner<T> {
    /// Spawns a new WAPM process by its name
    fn spawn(&mut self, name: &str, config: &SpawnOptionsConfig<T>) -> Result<BusSpawnedProcess<T>>;
}

#[derive(Debug, Clone)]
pub enum SpawnType<T> {
    New {
        chroot: bool,
        args: Vec<String>,
        preopen: Vec<String>,
        stdin_mode: StdioMode,
        stdout_mode: StdioMode,
        stderr_mode: StdioMode,
        working_dir: Option<String>,
    },
    Existing {
        env: T,
    }
}

#[derive(Debug, Clone)]
pub struct SpawnOptionsConfig<T> {
    reuse: bool,
    spawn_type: SpawnType<T>,
    remote_instance: Option<String>,
    access_token: Option<String>,
}

pub trait SpawnEnvironmentIntrinsics {
    fn chroot(&self) -> bool;

    fn args(&self) -> &Vec<String>;

    fn preopen(&self) -> &Vec<String>;

    fn stdin_mode(&self) -> StdioMode;

    fn stdout_mode(&self) -> StdioMode;

    fn stderr_mode(&self) -> StdioMode;

    fn working_dir(&self) -> String;
}

impl<T> SpawnOptionsConfig<T>
where T: SpawnEnvironmentIntrinsics
{
    pub fn reuse(&self) -> bool {
        self.reuse
    }

    pub fn spawn_type(&self) -> &SpawnType<T> {
        &self.spawn_type
    }

    pub fn env(&self) -> Option<&T> {
        match &self.spawn_type {
            SpawnType::New { .. } => None,
            SpawnType::Existing { env } => Some(&env)
        }
    }

    pub fn chroot(&self) -> bool {
        match &self.spawn_type {
            SpawnType::New { chroot, .. } => *chroot,
            SpawnType::Existing { env } => env.chroot()
        }
    }

    pub fn args(&self) -> &Vec<String> {
        match &self.spawn_type {
            SpawnType::New { args, .. } => args,
            SpawnType::Existing { env } => env.args()
        }
    }

    pub fn preopen(&self) -> &Vec<String> {
        match &self.spawn_type {
            SpawnType::New { preopen, .. } => preopen,
            SpawnType::Existing { env } => env.preopen()
        }
    }

    pub fn stdin_mode(&self) -> StdioMode {
        match &self.spawn_type {
            SpawnType::New { stdin_mode, .. } => *stdin_mode,
            SpawnType::Existing { env } => env.stdin_mode()
        }
    }

    pub fn stdout_mode(&self) -> StdioMode {
        match &self.spawn_type {
            SpawnType::New { stdout_mode, .. } => *stdout_mode,
            SpawnType::Existing { env } => env.stdout_mode()
        }
    }

    pub fn stderr_mode(&self) -> StdioMode {
        match &self.spawn_type {
            SpawnType::New { stderr_mode, .. } => *stderr_mode,
            SpawnType::Existing { env } => env.stderr_mode()
        }
    }

    pub fn working_dir(&self) -> Option<String> {
        match &self.spawn_type {
            SpawnType::New { working_dir, .. } => working_dir.as_ref().map(|a| a.clone()),
            SpawnType::Existing { env } => Some(env.working_dir())
        }
    }

    pub fn remote_instance(&self) -> Option<&str> {
        self.remote_instance.as_deref()
    }

    pub fn access_token(&self) -> Option<&str> {
        self.access_token.as_deref()
    }
}

pub struct SpawnOptions<T> {
    spawner: Box<dyn VirtualBusSpawner<T>>,
    conf: SpawnOptionsConfig<T>,
}

impl<T> SpawnOptions<T>
where T: SpawnEnvironmentIntrinsics
{
    pub fn new(spawner: Box<dyn VirtualBusSpawner<T>>, spawn_type: SpawnType<T>) -> Self {
        Self {
            spawner,
            conf: SpawnOptionsConfig {
                reuse: false,
                spawn_type,
                remote_instance: None,
                access_token: None,
            },
        }
    }

    pub fn options(&mut self, options: SpawnOptionsConfig<T>) -> &mut Self {
        self.conf = options;
        self
    }

    /// Spawns a new bus instance by its reference name
    pub fn spawn(&mut self, name: &str) -> Result<BusSpawnedProcess<T>> {
        self.spawner.spawn(name, &self.conf)
    }
}

#[derive(Debug)]
pub struct BusSpawnedProcess<T> {
    /// Name of the spawned process
    pub name: String,
    /// Configuration applied to this spawned thread
    pub config: SpawnOptionsConfig<T>,
    /// Reference to the spawned instance
    pub inst: Box<dyn VirtualBusProcess + Sync + Unpin>,
    /// Virtual file used for stdin
    pub stdin: Option<Box<dyn VirtualFile + Send + Sync + 'static>>,
    /// Virtual file used for stdout
    pub stdout: Option<Box<dyn VirtualFile + Send + Sync + 'static>>,
    /// Virtual file used for stderr
    pub stderr: Option<Box<dyn VirtualFile + Send + Sync + 'static>>,
}

pub trait VirtualBusScope: fmt::Debug + Send + Sync + 'static {
    //// Returns true if the invokable target has finished
    fn poll_finished(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<()>;
}

pub trait VirtualBusInvokable: fmt::Debug + Send + Sync + 'static {
    /// Invokes a service within this instance
    fn invoke(
        &self,
        topic_hash: u128,
        format: BusDataFormat,
        buf: Vec<u8>,
    ) -> Box<dyn VirtualBusInvoked>;
}

pub trait VirtualBusInvoked: fmt::Debug + Unpin + 'static {
    //// Returns once the bus has been invoked (or failed)
    fn poll_invoked(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Result<Box<dyn VirtualBusInvocation + Sync>>>;
}

pub trait VirtualBusProcess:
    VirtualBusScope + VirtualBusInvokable + fmt::Debug + Send + Sync + 'static
{
    /// Returns the exit code if the instance has finished
    fn exit_code(&self) -> Option<u32>;

    /// Polls to check if the process is ready yet to receive commands
    fn poll_ready(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<()>;
}

pub trait VirtualBusInvocation:
    VirtualBusInvokable + fmt::Debug + Send + Sync + Unpin + 'static
{
    /// Polls for new listen events related to this context
    fn poll_event(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<BusInvocationEvent>;
}

#[derive(Debug)]
pub struct InstantInvocation
{
    val: Option<BusInvocationEvent>,
    err: Option<VirtualBusError>,
    call: Option<Box<dyn VirtualBusInvocation + Sync>>,
}

impl InstantInvocation
{
    pub fn response(format: BusDataFormat, data: Vec<u8>) -> Self {
        Self {
            val: Some(BusInvocationEvent::Response { format, data }),
            err: None,
            call: None
        }
    }

    pub fn fault(err: VirtualBusError) -> Self {
        Self {
            val: None,
            err: Some(err),
            call: None
        }
    }

    pub fn call(val: Box<dyn VirtualBusInvocation + Sync>) -> Self {
        Self {
            val: None,
            err: None,
            call: Some(val)
        }
    }
}

impl VirtualBusInvoked
for InstantInvocation
{
    fn poll_invoked(mut self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<Result<Box<dyn VirtualBusInvocation + Sync>>> {
        if let Some(err) = self.err.take() {
            return Poll::Ready(Err(err));
        }
        if let Some(val) = self.val.take() {
            return Poll::Ready(Ok(Box::new(InstantInvocation {
                val: Some(val),
                err: None,
                call: None,
            })));
        }
        match self.call.take() {
            Some(val) => {
                Poll::Ready(Ok(val))
            },
            None => {
                Poll::Ready(Err(VirtualBusError::AlreadyConsumed))
            }
        }
    }
}

impl VirtualBusInvocation
for InstantInvocation
{
    fn poll_event(mut self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<BusInvocationEvent> {
        match self.val.take() {
            Some(val) => {
                Poll::Ready(val)
            },
            None => {
                Poll::Ready(BusInvocationEvent::Fault { fault: VirtualBusError::AlreadyConsumed })
            }
        }
    }
}

impl VirtualBusInvokable
for InstantInvocation
{
    fn invoke(
        &self,
        _topic_hash: u128,
        _format: BusDataFormat,
        _buf: Vec<u8>,
    ) -> Box<dyn VirtualBusInvoked> {
        Box::new(
            InstantInvocation {
                val: None,
                err: Some(VirtualBusError::InvalidTopic),
                call: None
            }
        )
    }
}

#[derive(Debug)]
pub enum BusInvocationEvent {
    /// The server has sent some out-of-band data to you
    Callback {
        /// Topic that this call relates to
        topic_hash: u128,
        /// Format of the data we received
        format: BusDataFormat,
        /// Data passed in the call
        data: Vec<u8>,
    },
    /// The service has a responded to your call
    Response {
        /// Format of the data we received
        format: BusDataFormat,
        /// Data returned by the call
        data: Vec<u8>,
    },
    /// The service has responded with a fault
    Fault {
        /// Fault code that was raised
        fault: VirtualBusError
    }
}

pub trait VirtualBusListener: fmt::Debug + Send + Sync + Unpin + 'static {
    /// Polls for new calls to this service
    fn poll(self: Pin<&Self>, cx: &mut Context<'_>) -> Poll<BusCallEvent>;
}

#[derive(Debug)]
pub struct BusCallEvent {
    /// Topic hash that this call relates to
    pub topic_hash: u128,
    /// Reference to the call itself
    pub called: Box<dyn VirtualBusCalled + Sync + Unpin>,
    /// Format of the data we received
    pub format: BusDataFormat,
    /// Data passed in the call
    pub data: Vec<u8>,
}

pub trait VirtualBusCalled: fmt::Debug + Send + Sync + 'static
{
    /// Polls for new calls to this service
    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<BusCallEvent>;

    /// Sends an out-of-band message back to the caller
    fn callback(&self, topic_hash: u128, format: BusDataFormat, buf: Vec<u8>);

    /// Informs the caller that their call has failed
    fn fault(self: Box<Self>, fault: VirtualBusError);

    /// Finishes the call and returns a particular response
    fn reply(&self, format: BusDataFormat, buf: Vec<u8>);
}

/// Format that the supplied data is in
#[derive(Debug, Copy, Clone, PartialEq, Eq)]
pub enum BusDataFormat {
    Raw,
    Bincode,
    MessagePack,
    Json,
    Yaml,
    Xml,
}

#[derive(Debug, Default)]
pub struct UnsupportedVirtualBus
{
}

impl<T> VirtualBus<T> for UnsupportedVirtualBus
where T: SpawnEnvironmentIntrinsics,
      T: std::fmt::Debug + Send + Sync + 'static
{
}

#[derive(Debug, Default)]
pub struct UnsupportedVirtualBusSpawner
{
}

impl<T> VirtualBusSpawner<T> for UnsupportedVirtualBusSpawner
where T: std::fmt::Debug + Send + Sync + 'static
{
    fn spawn(&mut self, _name: &str, _config: &SpawnOptionsConfig<T>) -> Result<BusSpawnedProcess<T>> {
        Err(VirtualBusError::Unsupported)
    }
}

#[derive(Error, Copy, Clone, Debug, PartialEq, Eq)]
pub enum VirtualBusError {
    /// Failed during serialization
    #[error("serialization failed")]
    Serialization,
    /// Failed during deserialization
    #[error("deserialization failed")]
    Deserialization,
    /// Invalid WAPM process
    #[error("invalid wapm")]
    InvalidWapm,
    /// Failed to fetch the WAPM process
    #[error("fetch failed")]
    FetchFailed,
    /// Failed to compile the WAPM process
    #[error("compile error")]
    CompileError,
    /// Invalid ABI
    #[error("WAPM process has an invalid ABI")]
    InvalidABI,
    /// Call was aborted
    #[error("call aborted")]
    Aborted,
    /// Bad handle
    #[error("bad handle")]
    BadHandle,
    /// Invalid topic
    #[error("invalid topic")]
    InvalidTopic,
    /// Invalid callback
    #[error("invalid callback")]
    BadCallback,
    /// Call is unsupported
    #[error("unsupported")]
    Unsupported,
    /// Bad request
    #[error("bad request")]
    BadRequest,
    /// Access denied
    #[error("access denied")]
    AccessDenied,
    /// Internal error has occured
    #[error("internal error")]
    InternalError,
    /// Memory allocation failed
    #[error("memory allocation failed")]
    MemoryAllocationFailed,
    /// Invocation has failed
    #[error("invocation has failed")]
    InvokeFailed,
    /// Already consumed
    #[error("already consumed")]
    AlreadyConsumed,
    /// Memory access violation
    #[error("memory access violation")]
    MemoryAccessViolation,
    /// Some other unhandled error. If you see this, it's probably a bug.
    #[error("unknown error found")]
    UnknownError,
}
