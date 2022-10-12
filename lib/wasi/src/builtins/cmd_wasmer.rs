use wasmer::{FunctionEnvMut, Store};
use wasmer_vbus::{
    SpawnOptionsConfig,
    BusSpawnedProcess
};
use wasmer_wasi_types::__WASI_ENOENT;
use std::{
    ops::Deref,
    sync::{
        Arc,
        RwLock
    },
    collections::HashMap
};

use crate::{
    WasiEnv,
    syscalls::stderr_write,
    WasiRuntimeImplementation,
    bin_factory::{
        BinaryPackage,
        CachedCompiledModules,
        spawn_exec
    },
};

const HELP: &'static str = r#"USAGE:
    wasmer <SUBCOMMAND>

OPTIONS:
    -h, --help       Print help information

SUBCOMMANDS:
    run            Run a WebAssembly file. Formats accepted: wasm, wat
"#;

const HELP_RUN: &'static str = r#"USAGE:
    wasmer run <FILE> [ARGS]...

ARGS:
    <FILE>       File to run
    <ARGS>...    Application arguments
"#;

use super::BuiltInCommand;

#[derive(Debug, Clone)]
pub struct CmdWasmer {
    cache: Arc<RwLock<HashMap<String, Option<BinaryPackage>>>>,
    cache_webc_dir: String,
    runtime: Arc<dyn WasiRuntimeImplementation + Send + Sync + 'static>,
    compiled_modules: Arc<CachedCompiledModules>,
}

impl CmdWasmer {
    pub fn new(cache: Arc<RwLock<HashMap<String, Option<BinaryPackage>>>>, cache_webc_dir: String, runtime: Arc<dyn WasiRuntimeImplementation + Send + Sync + 'static>, compiled_modules: Arc<CachedCompiledModules>) -> Self {
        Self {
            cache,
            cache_webc_dir,
            runtime,
            compiled_modules
        }
    }
}

impl CmdWasmer{
    fn run<'a>(&self, parent_ctx: &FunctionEnvMut<'a, WasiEnv>, name: &str, store: Store, mut config: SpawnOptionsConfig<WasiEnv>, what: Option<String>, mut args: Vec<String>) -> wasmer_vbus::Result<BusSpawnedProcess> {
        if let Some(what) = what {
            // Set the arguments of the environment by replacing the state
            let mut state = config.env().state.fork();
            args.insert(0, what.clone());
            state.args = args;
            config.env_mut().state = Arc::new(state);

            // Get the binary
            if let Some(binary) = self.get(what.clone())
            {
                // Now run the module
                spawn_exec(binary, name, store, config, &self.runtime, &self.compiled_modules)
            } else {
                let _ = stderr_write(parent_ctx, format!("package not found - {}\r\n", what).as_bytes());
                Ok(BusSpawnedProcess::exited_process(name, __WASI_ENOENT as u32))   
            }
        } else {
            let _ = stderr_write(parent_ctx, HELP_RUN.as_bytes());
            Ok(BusSpawnedProcess::exited_process(name, 0))
        }
    }

    pub fn get(&self, name: String) -> Option<BinaryPackage>
    {
        // Fast path
        {
            let cache = self.cache.read().unwrap();
            if let Some(data) = cache.get(&name) {
                return data.clone();
            }
        }

        // Slow path
        let mut cache = self.cache.write().unwrap();

        // Check the cache
        if let Some(data) = cache.get(&name) {
            return data.clone();
        }

        // Now try for the WebC
        let wapm_name = name.split_once(":").map(|a| a.0).unwrap_or_else(|| name.as_str());
        let cache_webc_dir = self.cache_webc_dir.as_str();
        if let Some(data) = crate::wapm::fetch_webc(cache_webc_dir, wapm_name, self.runtime.deref()) {
            cache.insert(name, Some(data.clone()));
            return Some(data);
        }

        // The WebC is not found
        None
    }
}

impl BuiltInCommand
for CmdWasmer {
    fn exec<'a>(&self, parent_ctx: &FunctionEnvMut<'a, WasiEnv>, name: &str, store: Store, config: SpawnOptionsConfig<WasiEnv>) -> wasmer_vbus::Result<BusSpawnedProcess>
    {
        // Read the command we want to run
        let mut args = config.env().state.args.iter().map(|a| a.as_str());
        let _alias = args.next();
        let cmd = args.next();

        // Check the command
        match cmd {
            Some("run") => {
                let what = args.next().map(|a| a.to_string());
                let args = args.map(|a| a.to_string()).collect();
                self.run(parent_ctx, name, store, config, what, args)
            },
            Some("--help") |
            None => {
                let _ = stderr_write(parent_ctx, HELP.as_bytes());
                Ok(BusSpawnedProcess::exited_process(name, 0))
            },            
            Some(what) => {
                let what = Some(what.to_string());
                let args = args.map(|a| a.to_string()).collect();
                self.run(parent_ctx, name, store, config, what, args)
            }
        }
    }
}