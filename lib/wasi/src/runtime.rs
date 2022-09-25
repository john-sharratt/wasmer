use std::fmt;
use std::ops::Deref;
use thiserror::Error;
use wasmer::{Module, Store};
use wasmer::vm::VMMemory;
use wasmer_vbus::{UnsupportedVirtualBus, VirtualBus};
use wasmer_vnet::VirtualNetworking;

use crate::{WasiCallingId, WasiEnv};

use super::types::*;
use super::WasiError;

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

/// Represents an implementation of the WASI runtime - by default everything is
/// unimplemented.
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
            rows: 25,
            cols: 80,
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
    fn tty_set(&self, _tty_state: WasiTtyState) {}

    /// Spawns a new thread by invoking the
    fn thread_spawn(
        &self,
        _callback: Box<dyn FnOnce(Store, Module, VMMemory) + Send + 'static>,
        _store: Store,
        _existing_module: Module,
        _existing_memory: VMMemory,
    ) -> Result<(), WasiThreadError>
    {
        Err(WasiThreadError::Unsupported)
    }

    /// Returns the amount of parallelism that is possible on this platform
    fn thread_parallelism(&self) -> Result<usize, WasiThreadError> {
        Err(WasiThreadError::Unsupported)
    }

    /// Invokes whenever a WASM thread goes idle. In some runtimes (like singlethreaded
    /// execution environments) they will need to do asynchronous work whenever the main
    /// thread goes idle and this is the place to hook for that.
    fn yield_now(&self, _id: WasiCallingId) -> Result<(), WasiError> {
        std::thread::yield_now();
        Ok(())
    }
}

#[derive(Debug)]
pub struct PluggableRuntimeImplementation
{
    pub bus: Box<dyn VirtualBus<WasiEnv> + Sync>,
    pub networking: Box<dyn VirtualNetworking + Sync>,
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
        Self {
            #[cfg(not(feature = "host-vnet"))]
            networking: Box::new(wasmer_vnet::UnsupportedVirtualNetworking::default()),
            #[cfg(feature = "host-vnet")]
            networking: Box::new(wasmer_wasi_local_networking::LocalNetworking::default()),
            bus: Box::new(UnsupportedVirtualBus::default()),
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
    fn thread_spawn(
        &self,
        callback: Box<dyn FnOnce(Store, Module, VMMemory) + Send + 'static>,
        store: Store,
        existing_module: Module,
        existing_memory: VMMemory,
    ) -> Result<(), WasiThreadError>
    {
        let existing_module = existing_module.clone();
        std::thread::spawn(move || {
            callback(store, existing_module, existing_memory)
        });
        Ok(())
    }

    fn thread_parallelism(&self) -> Result<usize, WasiThreadError> {
        std::thread::available_parallelism()
            .map(|a| usize::from(a))
            .map_err(|_| WasiThreadError::Unsupported)
    }
}
