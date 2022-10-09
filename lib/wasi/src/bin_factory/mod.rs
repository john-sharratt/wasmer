use std::{
    sync::{
        Arc,
        RwLock
    },
    ops::{
        Deref
    },
    collections::{
        HashMap
    }, path::PathBuf,
};
use derivative::Derivative;

mod binary_package;
mod cached_modules;
mod exec;

pub use binary_package::*;
pub use cached_modules::*;
pub use exec::*;

use sha2::*;
use wasmer::{AsStoreRef, Module};

use crate::{
    WasiState,
    WasiRuntimeImplementation
};

pub const DEFAULT_WEBC_PATH: &'static str = "~/.wasmer/webc";

#[derive(Derivative, Clone)]
pub struct BinFactory {
    state: Arc<WasiState>,
    cache: Arc<RwLock<HashMap<String, Option<BinaryPackage>>>>,
    cache_webc_dir: String,
    runtime: Arc<dyn WasiRuntimeImplementation + Send + Sync + 'static>,
    pub(crate) compiled_modules: Arc<CachedCompiledModules>,
}

impl BinFactory {
    pub fn new(
        state: Arc<WasiState>,
        compiled_modules: Arc<CachedCompiledModules>,
        cache_webc_dir: Option<String>,
        runtime: Arc<dyn WasiRuntimeImplementation + Send + Sync + 'static>,
    ) -> BinFactory {
        let cache_webc_dir = shellexpand::tilde(cache_webc_dir
            .as_ref()
            .map(|a| a.as_str())
            .unwrap_or_else(|| DEFAULT_WEBC_PATH))
            .to_string();
        let _ = std::fs::create_dir_all(PathBuf::from(cache_webc_dir.clone()));
        BinFactory {
            state,
            cache: Arc::new(RwLock::new(HashMap::new())),
            cache_webc_dir,
            runtime,
            compiled_modules
        }
    }

    pub fn cache_webc_dir(&self) -> String {
        self.cache_webc_dir.clone()
    }

    pub fn runtime(&self) -> &dyn WasiRuntimeImplementation {
        self.runtime.deref()
    }

    pub fn clear(&self) {
        self.cache.write().unwrap().clear();
    }

    pub fn get(&self, name: &str) -> Option<BinaryPackage> {
        let name = name.to_string();

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
        
        // Check the filesystem for the file
        if name.starts_with("/") {
            if let Ok(mut file) = self.state
                .fs_new_open_options()
                .read(true)
                .open(name.clone())
            {
                // Read the file
                let mut data = Vec::with_capacity(file.size() as usize);
                if let Ok(_) = file.read_to_end(&mut data)
                {
                    let data = BinaryPackage::new(data.into());
                    cache.insert(name, Some(data.clone()));
                    return Some(data);
                }
            }
        }

        // Now try for the WebC
        if name.starts_with("@") {
            let wapm_name = &name[1..];
            let cache_webc_dir = self.cache_webc_dir.as_str();
            if let Some(data) = crate::wapm::fetch_webc(cache_webc_dir, wapm_name, self.runtime()) {
                cache.insert(name, Some(data.clone()));
                return Some(data);
            }
        }

        // NAK
        cache.insert(name, None);
        return None;
    }

    pub(crate) fn get_compiled_module(&self, store: &impl AsStoreRef, data_hash: &str, engine: &str) -> Option<Module> {
        self.compiled_modules
            .get_compiled_module(store, data_hash, engine)
    }

    pub(crate) fn set_compiled_module(&self, data_hash: &str, engine: &str, compiled_module: &Module) {
        self.compiled_modules
            .set_compiled_module(data_hash, engine, compiled_module)
    }
}

pub fn hash_of_binary(data: impl AsRef<[u8]>) -> String {
    let mut hasher = Sha256::default();
    hasher.update(data.as_ref());
    let hash = hasher.finalize();
    hex::encode(&hash[..])
}
