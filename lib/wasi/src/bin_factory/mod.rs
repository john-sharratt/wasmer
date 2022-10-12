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
pub(crate) use exec::spawn_exec;

use sha2::*;

use crate::{
    WasiState,
    WasiRuntimeImplementation, builtins::BuiltIns
};

pub const DEFAULT_WEBC_PATH: &'static str = "~/.wasmer/webc";

#[derive(Derivative, Clone)]
pub struct BinFactory {
    pub(crate) state: Arc<WasiState>,
    pub(crate) builtins: BuiltIns,
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
        let cache = Arc::new(RwLock::new(HashMap::new()));
        let cache_webc_dir = shellexpand::tilde(cache_webc_dir
            .as_ref()
            .map(|a| a.as_str())
            .unwrap_or_else(|| DEFAULT_WEBC_PATH))
            .to_string();
        let _ = std::fs::create_dir_all(PathBuf::from(cache_webc_dir.clone()));
        BinFactory {
            state,
            builtins: BuiltIns::new(cache.clone(), cache_webc_dir.clone(), runtime.clone(), compiled_modules.clone()),
            cache,
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

        // NAK
        cache.insert(name, None);
        return None;
    }
}

pub fn hash_of_binary(data: impl AsRef<[u8]>) -> String {
    let mut hasher = Sha256::default();
    hasher.update(data.as_ref());
    let hash = hasher.finalize();
    hex::encode(&hash[..])
}
