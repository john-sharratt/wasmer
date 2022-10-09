use std::{
    collections::HashMap,
    cell::RefCell,
    ops::DerefMut,
    sync::{
        RwLock,
    },
    path::PathBuf
};

use bytes::Bytes;
use wasmer::{Module, AsStoreRef};

pub const DEFAULT_COMPILED_PATH: &'static str = "~/.wasmer/compiled";

#[derive(Debug)]
pub struct CachedCompiledModules {
    #[cfg(feature = "sys")]
    modules: RwLock<HashMap<String, Module>>,
    cache_compile_dir: String,
}

impl Default
for CachedCompiledModules {
    fn default() -> Self {
        CachedCompiledModules::new(None)
    }
}

thread_local! {
    static THREAD_LOCAL_CACHED_MODULES: std::cell::RefCell<HashMap<String, Module>> 
        = RefCell::new(HashMap::new());
}

impl CachedCompiledModules
{
    pub fn new(cache_compile_dir: Option<String>) -> CachedCompiledModules {
        let cache_compile_dir = shellexpand::tilde(cache_compile_dir
            .as_ref()
            .map(|a| a.as_str())
            .unwrap_or_else(|| DEFAULT_COMPILED_PATH))
            .to_string();
        let _ = std::fs::create_dir_all(PathBuf::from(cache_compile_dir.clone()));
        CachedCompiledModules {
            #[cfg(feature = "sys")]
            modules: RwLock::new(HashMap::default()),
            cache_compile_dir,
        }
    }

    pub fn get_compiled_module(&self, store: &impl AsStoreRef, data_hash: &str, compiler: &str) -> Option<Module> {
        let key = format!("{}-{}", data_hash, compiler);
        
        // fastest path
        {
            let module = THREAD_LOCAL_CACHED_MODULES.with(|cache| {
                let cache = cache.borrow();
                cache.get(&key).map(|m| m.clone())
            });
            if let Some(module) = module {
                return Some(module);
            }
        }

        // fast path
        #[cfg(feature = "sys")]
        {
            let cache = self.modules.read().unwrap();
            if let Some(module) = cache.get(&key) {
                THREAD_LOCAL_CACHED_MODULES.with(|cache| {
                    let mut cache = cache.borrow_mut();
                    cache.insert(key.clone(), module.clone());
                });
                return Some(module.clone());
            }
        }

        // slow path
        let path = std::path::Path::new(self.cache_compile_dir.as_str()).join(format!("{}.bin", key).as_str());
        if let Ok(data) = std::fs::read(path) {
            let mut decoder = weezl::decode::Decoder::new(weezl::BitOrder::Msb, 8);
            if let Ok(data) = decoder.decode(&data[..]) {
                let module_bytes = Bytes::from(data);

                // Load the module
                let module = unsafe { Module::deserialize(store, &module_bytes[..])
                    .unwrap()
                };

                #[cfg(feature = "sys")]
                {
                    let mut cache = self.modules.write().unwrap();
                    cache.insert(key.clone(), module.clone());
                }
                THREAD_LOCAL_CACHED_MODULES.with(|cache| {
                    let mut cache = cache.borrow_mut();
                    cache.insert(key.clone(), module.clone());
                });
                return Some(module);
            }
        }

        // Not found
        None
    }

    pub fn set_compiled_module(&self, data_hash: &str, compiler: &str, module: &Module) {
        let key = format!("{}-{}", data_hash, compiler);
        
        // Add the module to the local thread cache
        THREAD_LOCAL_CACHED_MODULES.with(|cache| {
            let mut cache = cache.borrow_mut();
            let cache = cache.deref_mut();
            cache.insert(key.clone(), module.clone());
        });

        // Serialize the compiled module into bytes and insert it into the cache
        #[cfg(feature = "sys")]
        {
            let mut cache = self.modules.write().unwrap();
            cache.insert(key.clone(), module.clone());
        }
        
        // We should also attempt to store it in the cache directory
        let compiled_bytes = module.serialize().unwrap();

        let path = std::path::Path::new(self.cache_compile_dir.as_str()).join(format!("{}.bin", key).as_str());
        let _ = std::fs::create_dir_all(path.parent().unwrap().clone());
        let mut encoder = weezl::encode::Encoder::new(weezl::BitOrder::Msb, 8);
        if let Ok(compiled_bytes) = encoder.encode(&compiled_bytes[..]) {
            let _ = std::fs::write(path, &compiled_bytes[..]);
        }
    }
}
