use wasmer::{Store, Module, Memory, Instance, FunctionEnvMut};
use wasmer_vbus::{
    VirtualBusSpawner,
    VirtualBusError,
    SpawnOptionsConfig,
    BusSpawnedProcess, VirtualBusProcess, VirtualBusScope, VirtualBusInvokable
};
use wasmer_wasi_types::__WASI_ENOEXEC;
use std::{
    pin::Pin,
    task::{
        Context,
        Poll
    }, sync::{Mutex, Arc}, ops::DerefMut
};
use tokio::sync::mpsc;
use tracing::*;

use crate::{WasiEnv, WasiFunctionEnv, runtime::SpawnedMemory, import_object_for_all_wasi_versions, WasiError, WasiRuntimeImplementation};
use super::{BinFactory, BinaryPackage, CachedCompiledModules};
use crate::runtime::SpawnType;

pub(crate) fn spawn_exec(binary: BinaryPackage, name: &str, store: Store, config: SpawnOptionsConfig<WasiEnv>, runtime: &Arc<dyn WasiRuntimeImplementation + Send + Sync + 'static>, compiled_modules: &CachedCompiledModules) -> wasmer_vbus::Result<BusSpawnedProcess> {
    let module = compiled_modules.get_compiled_module(
        &store,
        binary.hash().as_str(),
        store.engine().name()
    );
    let module = match module {
        Some(a) => a,
        None => {
            let module = Module::new(&store, &binary.entry[..])
                .map_err(|err| {
                    error!("failed to compile module [{}, len={}] - {}", name, binary.entry.len(), err);
                    VirtualBusError::CompileError
                })?;
            compiled_modules.set_compiled_module(
                binary.hash().as_str(),
                store.engine().name(),
                &module
            );
            module
        }
    };

    let (exit_code_tx, exit_code_rx) = mpsc::channel(1);
    {
        // We pass in a bunch of variables
        let module_name = name.to_string();

        // Determine if shared memory needs to be created and imported
        let shared_memory = module
            .imports()
            .memories()
            .next()
            .map(|a| *a.ty());

        // Determine if we are going to create memory and import it or just rely on self creation of memory
        let memory_spawn = match shared_memory {
            Some(ty) => {
                #[cfg(feature = "sys")]
                let style = store
                    .tunables()
                    .memory_style(&ty);            
                SpawnType::CreateWithType(SpawnedMemory {
                    ty,
                    #[cfg(feature = "sys")]
                    style
                })
            },
            None => SpawnType::Create,
        };

        // Create a thread that will run this process
        let runtime = runtime.clone();
        let runtime_outer = runtime.clone();
        runtime_outer.task_wasm(Box::new(move |mut store, module, memory| Box::pin(
            async move {
                // Create the WasiFunctionEnv
                let mut wasi_env = config.env().clone();
                wasi_env.runtime = runtime.clone();
                let mut wasi_env = WasiFunctionEnv::new(&mut store, wasi_env);

                // Let's instantiate the module with the imports.
                let mut import_object = import_object_for_all_wasi_versions(&mut store, &wasi_env.env);
                if let Some(memory) = memory {
                    import_object.define("env", "memory", Memory::new_from_existing(&mut store, memory));
                }
                let instance = match Instance::new(&mut store, &module, &import_object) {
                    Ok(a) => a,
                    Err(err) => {
                        error!("wasm instantiate error ({})", err);
                        return;
                    }
                };

                // Initialize the WASI environment
                if let Err(err) = wasi_env.initialize(&mut store, &instance) {
                    error!("wasi initialize error ({})", err);
                    return;
                }
                
                // If this module exports an _initialize function, run that first.
                if let Ok(initialize) = instance.exports.get_function("_initialize") {
                    if let Err(e) = initialize.call(&mut store, &[]) {
                        let code = match e.downcast::<WasiError>() {
                            Ok(WasiError::Exit(code)) => code,
                            Ok(WasiError::UnknownWasiVersion) => {
                                debug!("exec-failed: unknown wasi version");
                                __WASI_ENOEXEC as u32
                            }
                            Err(err) => {
                                debug!("exec-failed: runtime error - {}", err);
                                9999u32
                            },
                        };
                        let _ = exit_code_tx.send(code).await;
                        return;
                    }
                }

                // Let's call the `_start` function, which is our `main` function in Rust.
                let start = instance
                    .exports
                    .get_function("_start")
                    .ok();

                // If there is a start function
                debug!("called main() on {}", module_name);
                let ret = if let Some(start) = start {
                    match start.call(&mut store, &[]) {
                        Ok(_) => 0,
                        Err(e) => {
                            match e.downcast::<WasiError>() {
                                Ok(WasiError::Exit(code)) => code,
                                Ok(WasiError::UnknownWasiVersion) => {
                                    debug!("exec-failed: unknown wasi version");
                                __WASI_ENOEXEC as u32
                                }
                                Err(err) => {
                                    debug!("exec-failed: runtime error - {}", err);
                                    9999u32
                                },
                            }
                        },
                    }
                } else {
                    debug!("exec-failed: missing _start function");
                    __WASI_ENOEXEC as u32
                };
                debug!("main() has exited on {} with {}", module_name, ret);

                // Send the result
                let _ = exit_code_tx.send(ret).await;
                drop(exit_code_tx);
            }
        )), store, module, memory_spawn)
        .map_err(|err| {
            error!("failed to launch module [{}] - {}", name, err);
            VirtualBusError::UnknownError
        })?
    };

    let inst = Box::new(
        SpawnedProcess {
            exit_code: Mutex::new(None),
            exit_code_rx: Mutex::new(exit_code_rx),
        }
    );
    Ok(
        BusSpawnedProcess {
            name: name.to_string(),
            inst,
            stdin: None,
            stdout: None,
            stderr: None
        }
    )
}

impl VirtualBusSpawner<WasiEnv>
for BinFactory
{
    fn spawn<'a>(&self, parent_ctx: Option<&FunctionEnvMut<'a, WasiEnv>>, name: &str, store: Store, config: SpawnOptionsConfig<WasiEnv>, _fallback: &dyn VirtualBusSpawner<WasiEnv>) -> wasmer_vbus::Result<BusSpawnedProcess> {
        if config.remote_instance().is_some() {
            return Err(VirtualBusError::Unsupported);
        }

        // We check for built in commands
        if let Some(parent_ctx) = parent_ctx {
            if self.builtins.exists(name) {
                return self.builtins.exec(parent_ctx, name, store, config);
            }
        }

        // Find the binary (or die trying) and make the spawn type
        let binary = self.get(name)
            .ok_or(VirtualBusError::NotFound)?;
        spawn_exec(binary, name, store, config, &self.runtime, &self.compiled_modules)
    }
}

#[derive(Debug)]
struct SpawnedProcess {
    exit_code: Mutex<Option<u32>>,
    exit_code_rx: Mutex<mpsc::Receiver<u32>>,
}

impl VirtualBusProcess
for SpawnedProcess {
    fn exit_code(&self) -> Option<u32>
    {
        let mut exit_code = self.exit_code.lock().unwrap();
        if let Some(exit_code) = exit_code.as_ref() {
            return Some(exit_code.clone());
        }
        let mut rx = self.exit_code_rx.lock().unwrap();
        match rx.try_recv() {
            Ok(code) => {
                exit_code.replace(code);
                Some(code)
            },
            Err(mpsc::error::TryRecvError::Disconnected) => {
                let code = 9999;
                exit_code.replace(code);
                Some(code)
            }
            _ => None
        }
    }

    fn poll_ready(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<()> {
        {
            let exit_code = self.exit_code.lock().unwrap();
            if exit_code.is_some() {
                return Poll::Ready(());
            }
        }
        let mut rx = self.exit_code_rx.lock().unwrap();
        let mut rx = Pin::new(rx.deref_mut());
        match rx.poll_recv(cx) {
            Poll::Ready(code) => {
                let code = code.unwrap_or(9999);
                {
                    let mut exit_code = self.exit_code.lock().unwrap();
                    exit_code.replace(code);
                }
                Poll::Ready(())
            },
            Poll::Pending => Poll::Pending
        }
    }
}

impl VirtualBusScope
for SpawnedProcess {
    fn poll_finished(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<()> {
        VirtualBusProcess::poll_ready(self, cx)
    }
}

impl VirtualBusInvokable
for SpawnedProcess { }
