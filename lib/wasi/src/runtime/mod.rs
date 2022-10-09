use std::io::Write;
use std::sync::Arc;
use std::{fmt, io};
use std::future::Future;
use std::ops::Deref;
use std::pin::Pin;
use thiserror::Error;
use wasmer::{Module, Store, MemoryType};
use wasmer::vm::VMMemory;
#[cfg(feature = "sys")]
use wasmer_types::MemoryStyle;
use wasmer_vbus::{DefaultVirtualBus, VirtualBus};
use wasmer_vnet::VirtualNetworking;
use derivative::Derivative;
use tracing::*;

use crate::{WasiCallingId, WasiEnv};

use super::types::*;
use super::WasiError;

mod checkpoint;
mod ws;

pub use checkpoint::*;
pub use ws::*;

pub use {
    WebSocketAbi,
};

#[cfg(feature = "sys-thread")]
use tokio::runtime::{
    Builder, Runtime
};

#[derive(Error, Debug)]
pub enum WasiThreadError {
    #[error("Multithreading is not supported")]
    Unsupported,
    #[error("The method named is not an exported function")]
    MethodNotFound,
    #[error("Failed to create the requested memory")]
    MemoryCreateFailed,
    /// This will happen if WASM is running in a thread has not been created by the spawn_wasm call
    #[error("WASM context is invalid")]
    InvalidWasmContext,
}

impl From<WasiThreadError> for __wasi_errno_t {
    fn from(a: WasiThreadError) -> __wasi_errno_t {
        match a {
            WasiThreadError::Unsupported => __WASI_ENOTSUP,
            WasiThreadError::MethodNotFound => __WASI_EINVAL,
            WasiThreadError::MemoryCreateFailed => __WASI_EFAULT,
            WasiThreadError::InvalidWasmContext => __WASI_ENOEXEC,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct WasiTtyState {
    pub cols: u32,
    pub rows: u32,
    pub width: u32,
    pub height: u32,
    pub stdin_tty: bool,
    pub stdout_tty: bool,
    pub stderr_tty: bool,
    pub echo: bool,
    pub line_buffered: bool,
}

#[derive(Debug)]
pub struct SpawnedMemory
{
    pub ty: MemoryType,
    #[cfg(feature = "sys")]
    pub style: MemoryStyle,
}

#[derive(Debug)]
pub enum SpawnType {
    Create,
    CreateWithType(SpawnedMemory),
    NewThread(VMMemory),
}

#[derive(Debug, Default)]
pub struct ReqwestOptions {
    pub gzip: bool,
    pub cors_proxy: Option<String>,
}

pub struct ReqwestResponse {
    pub pos: usize,
    pub data: Option<Vec<u8>>,
    pub ok: bool,
    pub redirected: bool,
    pub status: u16,
    pub status_text: String,
    pub headers: Vec<(String, String)>,
}

/// Represents an implementation of the WASI runtime - by default everything is
/// unimplemented.
#[allow(unused_variables)]
pub trait WasiRuntimeImplementation
where Self: fmt::Debug + Sync,
{
    /// For WASI runtimes that support it they can implement a message BUS implementation
    /// which allows runtimes to pass serialized messages between each other similar to
    /// RPC's. BUS implementation can be implemented that communicate across runtimes
    /// thus creating a distributed computing architecture.
    fn bus(&self) -> &(dyn VirtualBus<WasiEnv>);

    /// Provides access to all the networking related functions such as sockets.
    /// By default networking is not implemented.
    fn networking(&self) -> &(dyn VirtualNetworking);

    /// Gets the TTY state
    fn tty_get(&self) -> WasiTtyState {
        WasiTtyState {
            rows: 80,
            cols: 25,
            width: 800,
            height: 600,
            stdin_tty: false,
            stdout_tty: false,
            stderr_tty: false,
            echo: true,
            line_buffered: true,
        }
    }

    /// Sets the TTY state
    fn tty_set(&self, _tty_state: WasiTtyState) {
    }

    /// Spawns a new thread by invoking the
    fn thread_spawn(
        &self,
        callback: Box<dyn FnOnce(Store, Module, VMMemory) + Send + 'static>,
        store: Store,
        existing_module: Module,
        existing_memory: VMMemory,
    ) -> Result<(), WasiThreadError>
    {
        self.task_wasm(Box::new(move |store, module, memory| {
                callback(store, module, memory.expect("failed to use existing memory"));
                Box::pin(async move { })
            }),
            store,
            existing_module,
            SpawnType::NewThread(existing_memory))
    }

    /// Returns the amount of parallelism that is possible on this platform
    fn thread_parallelism(&self) -> Result<usize, WasiThreadError> {
        Err(WasiThreadError::Unsupported)
    }

    /// Invokes whenever a WASM thread goes idle. In some runtimes (like singlethreaded
    /// execution environments) they will need to do asynchronous work whenever the main
    /// thread goes idle and this is the place to hook for that.
    fn sleep_now(&self, _id: WasiCallingId, ms: u128) -> Result<(), WasiError> {
        if ms == 0 {
            std::thread::yield_now();
        } else {
            std::thread::sleep(std::time::Duration::from_millis(ms as u64));
        }
        Ok(())
    }

    /// Starts an asynchronous task that will run on a shared worker pool
    /// This task must not block the execution or it could cause a deadlock
    fn task_shared(
        &self,
        task: Box<
            dyn FnOnce() -> Pin<Box<dyn Future<Output = ()> + Send + 'static>> + Send + 'static,
        >,
    ) -> Result<(), WasiThreadError> {
        Err(WasiThreadError::Unsupported)
    }

    /// Starts an asynchronous task will will run on a dedicated thread
    /// pulled from the worker pool that has a stateful thread local variable
    /// It is ok for this task to block execution and any async futures within its scope
    fn task_wasm(
        &self,
        task: Box<dyn FnOnce(Store, Module, Option<VMMemory>) -> Pin<Box<dyn Future<Output = ()> + 'static>> + Send + 'static>,
        store: Store,
        module: Module,
        spawn_type: SpawnType,
    ) -> Result<(), WasiThreadError> {
        Err(WasiThreadError::Unsupported)
    }

    /// Starts an asynchronous task will will run on a dedicated thread
    /// pulled from the worker pool. It is ok for this task to block execution
    /// and any async futures within its scope
    fn task_dedicated(
        &self,
        task: Box<dyn FnOnce() + Send + 'static>,
    ) -> Result<(), WasiThreadError> {
        Err(WasiThreadError::Unsupported)
    }

    /// Starts an asynchronous task will will run on a dedicated thread
    /// pulled from the worker pool. It is ok for this task to block execution
    /// and any async futures within its scope
    fn task_dedicated_async(
        &self,
        task: Box<dyn FnOnce() -> Pin<Box<dyn Future<Output = ()> + 'static>> + Send + 'static>,
    ) -> Result<(), WasiThreadError> {
        Err(WasiThreadError::Unsupported)
    }

    /// Performs a HTTP or HTTPS request to a destination URL
    fn reqwest(
        &self,
        url: &str,
        method: &str,
        options: ReqwestOptions,
        headers: Vec<(String, String)>,
        data: Option<Vec<u8>>,
    ) -> Result<ReqwestResponse, u32> {
        Err(__WASI_ENOTSUP as u32)
    }

    /// Make a web socket connection to a particular URL
    fn web_socket(&self, url: &str) -> Result<Box<dyn WebSocketAbi>, String> {
        Err("not supported".to_string())
    }

    /// Writes output to the console
    fn stdout(&self, data: &[u8]) -> io::Result<()> {
        let mut handle = io::stdout();
        handle.write_all(data)
    }

    /// Writes output to the console
    fn stderr(&self, data: &[u8]) -> io::Result<()> {
        let mut handle = io::stderr();
        handle.write_all(data)
    }

    /// Flushes the output to the console
    fn flush(&self) -> io::Result<()> {
        io::stdout().flush()?;
        io::stderr().flush()?;
        Ok(())
    }

    /// Writes output to the log
    fn log(&self, text: String) -> io::Result<()> {
        self.stderr(text.as_bytes())
    }

    /// Clears the terminal
    fn cls(&self) -> io::Result<()> {
        self.stdout("\x1B[H\x1B[2J".as_bytes())
    }
}

#[derive(Derivative)]
#[derivative(Debug)]
pub struct PluggableRuntimeImplementation
{
    pub bus: Box<dyn VirtualBus<WasiEnv> + Sync>,
    pub networking: Box<dyn VirtualNetworking + Sync>,
    #[cfg(feature = "sys-thread")]
    runtime: Arc<Runtime>,
}

impl PluggableRuntimeImplementation
{
    pub fn set_bus_implementation<I>(&mut self, bus: I)
    where
        I: VirtualBus<WasiEnv> + Sync,
    {
        self.bus = Box::new(bus)
    }

    pub fn set_networking_implementation<I>(&mut self, net: I)
    where
        I: VirtualNetworking + Sync,
    {
        self.networking = Box::new(net)
    }
}

impl Default
for PluggableRuntimeImplementation
{
    fn default() -> Self {
        #[cfg(feature = "sys-thread")]
        let runtime = Builder::new_multi_thread().enable_all().build().unwrap();
        Self {
            #[cfg(not(feature = "host-vnet"))]
            networking: Box::new(wasmer_vnet::UnsupportedVirtualNetworking::default()),
            #[cfg(feature = "host-vnet")]
            networking: Box::new(wasmer_wasi_local_networking::LocalNetworking::default()),
            bus: Box::new(DefaultVirtualBus::default()),
            #[cfg(feature = "sys-thread")]
            runtime: Arc::new(runtime)
        }
    }
}

impl WasiRuntimeImplementation
for PluggableRuntimeImplementation
{
    fn bus<'a>(&'a self) -> &'a (dyn VirtualBus<WasiEnv>) {
        self.bus.deref()
    }

    fn networking<'a>(&'a self) -> &'a (dyn VirtualNetworking) {
        self.networking.deref()
    }

    #[cfg(feature = "sys-thread")]
    fn task_shared(
        &self,
        task: Box<
            dyn FnOnce() -> Pin<Box<dyn Future<Output = ()> + Send + 'static>> + Send + 'static,
        >,
    ) -> Result<(), WasiThreadError> {
        self.runtime.spawn(async move {
            let fut = task();
            fut.await
        });
        Ok(())
    }

    #[cfg(feature = "sys-thread")]
    fn task_wasm(
        &self,
        task: Box<dyn FnOnce(Store, Module, Option<VMMemory>) -> Pin<Box<dyn Future<Output = ()> + 'static>> + Send + 'static>,
        store: Store,
        module: Module,
        spawn_type: SpawnType,
    ) -> Result<(), WasiThreadError> {
        use wasmer::vm::VMSharedMemory;

        let memory: Option<VMMemory> = match spawn_type {
            SpawnType::CreateWithType(mem) => {
                Some(
                    VMSharedMemory::new(&mem.ty, &mem.style)
                        .map_err(|err| {
                            error!("failed to create memory - {}", err);
                        })
                        .unwrap()
                        .into()
                )
            },
            SpawnType::NewThread(mem) => Some(mem),
            SpawnType::Create => None,
        };
        
        let rt = self.runtime.clone();
        self.runtime.spawn_blocking(move || {
            // Invoke the callback
            let fut = task(store, module, memory);
            rt.block_on(fut)
        });
        Ok(())
    }

    #[cfg(feature = "sys-thread")]
    fn task_dedicated(
        &self,
        task: Box<dyn FnOnce() + Send + 'static>,
    ) -> Result<(), WasiThreadError> {
        self.runtime.spawn_blocking(move || {
            task();
        });
        Ok(())
    }

    #[cfg(feature = "sys-thread")]
    fn task_dedicated_async(
        &self,
        task: Box<dyn FnOnce() -> Pin<Box<dyn Future<Output = ()> + 'static>> + Send + 'static>,
    ) -> Result<(), WasiThreadError> {
        let rt = self.runtime.clone();
        self.runtime.spawn_blocking(move || {
            let fut = task();
            rt.block_on(fut)
        });
        Ok(())
    }

    #[cfg(feature = "sys-thread")]
    fn thread_parallelism(&self) -> Result<usize, WasiThreadError> {
        Ok(
            std::thread::available_parallelism()
                .map(|a| usize::from(a))
                .unwrap_or(8)
        )
    }

    /// Performs a HTTP or HTTPS request to a destination URL
    #[cfg(feature = "host-reqwest")]
    fn reqwest(
        &self,
        url: &str,
        method: &str,
        _options: ReqwestOptions,
        headers: Vec<(String, String)>,
        data: Option<Vec<u8>>,
    ) -> Result<ReqwestResponse, u32> {
        use std::convert::TryFrom;

        let method = reqwest::Method::try_from(method).map_err(|err| {
            debug!("failed to convert method ({}) - {}", method, err);
            __WASI_EIO as u32
        })?;

        let client = reqwest::blocking::ClientBuilder::default().build().map_err(|err| {
            debug!("failed to build reqwest client - {}", err);
            __WASI_EIO as u32
        })?;

        let mut builder = client.request(method, url);
        for (header, val) in headers {
            if let Ok(header) =
                reqwest::header::HeaderName::from_bytes(header.as_bytes())
            {
                builder = builder.header(header, val);
            } else {
                debug!("failed to parse header - {}", header);
            }
        }

        if let Some(data) = data {
            builder = builder.body(reqwest::blocking::Body::from(data));
        }

        let request = builder.build().map_err(|err| {
            debug!("failed to convert request (url={}) - {}", url, err);
            __WASI_EIO as u32
        })?;

        let response = client.execute(request).map_err(|err| {
            debug!("failed to execute reqest - {}", err);
            __WASI_EIO as u32
        })?;

        let status = response.status().as_u16();
        let status_text = response.status().as_str().to_string();
        let data = response.bytes().map_err(|err| {
            debug!("failed to read response bytes - {}", err);
            __WASI_EIO as u32
        })?;
        let data = data.to_vec();

        Ok(ReqwestResponse {
            pos: 0usize,
            ok: true,
            status,
            status_text,
            redirected: false,
            data: Some(data),
            headers: Vec::new(),
        })
    }
}
