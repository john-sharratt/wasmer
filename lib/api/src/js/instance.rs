use crate::js::env::HostEnvInitError;
use crate::js::export::Export;
use crate::js::exports::{Exportable, Exports};
use crate::js::externals::Extern;
use crate::js::imports::Imports;
use crate::js::module::Module;
use crate::js::store::Store;
use crate::js::trap::RuntimeError;
use crate::js::thread::ThreadControl;
use crate::js::Reactors;
use js_sys::WebAssembly;
use std::fmt;
use std::sync::{Arc, atomic::{AtomicU32, Ordering}};
#[cfg(feature = "std")]
use thiserror::Error;

/// A WebAssembly Instance is a stateful, executable
/// instance of a WebAssembly [`Module`].
///
/// Instance objects contain all the exported WebAssembly
/// functions, memories, tables and globals that allow
/// interacting with WebAssembly.
///
/// Spec: <https://webassembly.github.io/spec/core/exec/runtime.html#module-instances>
#[derive(Clone)]
pub struct Instance {
    thread_seed: Arc<AtomicU32>,
    instance: WebAssembly::Instance,
    module: Module,
    /// Reactors are used to wake up the program
    pub reactors: Reactors,
    imports: Imports,
    /// The exports for an instance.
    pub exports: Exports,
}

/// An error while instantiating a module.
///
/// This is not a common WebAssembly error, however
/// we need to differentiate from a `LinkError` (an error
/// that happens while linking, on instantiation), a
/// Trap that occurs when calling the WebAssembly module
/// start function, and an error when initializing the user's
/// host environments.
#[derive(Debug)]
#[cfg_attr(feature = "std", derive(Error))]
pub enum InstantiationError {
    /// A linking ocurred during instantiation.
    #[cfg_attr(feature = "std", error("Link error: {0}"))]
    Link(String),

    /// A runtime error occured while invoking the start function
    #[cfg_attr(feature = "std", error(transparent))]
    Start(RuntimeError),

    /// Error occurred when initializing the host environment.
    #[cfg_attr(feature = "std", error(transparent))]
    HostEnvInitialization(HostEnvInitError),
}

#[cfg(feature = "core")]
impl std::fmt::Display for InstantiationError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "InstantiationError")
    }
}

impl Instance {
    /// Creates a new `Instance` from a WebAssembly [`Module`] and a
    /// set of imports resolved by the [`Resolver`].
    ///
    /// The resolver can be anything that implements the [`Resolver`] trait,
    /// so you can plug custom resolution for the imports, if you wish not
    /// to use [`ImportObject`].
    ///
    /// The [`ImportObject`] is the easiest way to provide imports to the instance.
    ///
    /// [`ImportObject`]: crate::js::ImportObject
    ///
    /// ```
    /// # use wasmer::{imports, Store, Module, Global, Value, Instance};
    /// # fn main() -> anyhow::Result<()> {
    /// let store = Store::default();
    /// let module = Module::new(&store, "(module)")?;
    /// let imports = imports!{
    ///   "host" => {
    ///     "var" => Global::new(&store, Value::I32(2))
    ///   }
    /// };
    /// let instance = Instance::new(&module, &imports)?;
    /// # Ok(())
    /// # }
    /// ```
    ///
    /// ## Errors
    ///
    /// The function can return [`InstantiationError`]s.
    ///
    /// Those are, as defined by the spec:
    ///  * Link errors that happen when plugging the imports into the instance
    ///  * Runtime errors that happen when running the module `start` function.
    pub fn new(module: &Module, imports: &Imports) -> Result<Self, InstantiationError> {
        let import_copy = imports.clone();
        let (instance, imports): (WebAssembly::Instance, Vec<Extern>) = module
            .instantiate(imports)
            .map_err(|e| InstantiationError::Start(e))?;

        let self_instance = Self::from_module_and_instance(module, instance, import_copy)?;
        self_instance.init_envs(&imports.iter().map(Extern::to_export).collect::<Vec<_>>())?;
        Ok(self_instance)
    }

    /// Creates a Wasmer `Instance` from a Wasmer `Module` and a WebAssembly Instance
    ///
    /// # Important
    ///
    /// Is expected that the function [`Instance::init_envs`] is run manually
    /// by the user in case the instance has any Wasmer imports, so the function
    /// environments are properly initiated.
    ///
    /// *This method is only available when targeting JS environments*
    pub fn from_module_and_instance(
        module: &Module,
        instance: WebAssembly::Instance,
        imports: Imports,
    ) -> Result<Self, InstantiationError> {
        let store = module.store();
        let instance_exports = instance.exports();
        let exports = module
            .exports()
            .map(|export_type| {
                let name = export_type.name();
                let extern_type = export_type.ty().clone();
                let js_export =
                    js_sys::Reflect::get(&instance_exports, &name.into()).map_err(|_e| {
                        InstantiationError::Link(format!(
                            "Can't get {} from the instance exports",
                            &name
                        ))
                    })?;
                let export: Export = (js_export, extern_type).into();
                let extern_ = Extern::from_vm_export(store, export);
                Ok((name.to_string(), extern_))
            })
            .collect::<Result<Exports, InstantiationError>>()?;

        Ok(Self {
            thread_seed: Arc::new(AtomicU32::new(0)),
            instance,
            reactors: Default::default(),
            module: module.clone(),
            imports,
            exports,
        })
    }

    /// Initialize the given extern imports with the `Instance`.
    ///
    /// # Important
    ///
    /// This method should be called if the Wasmer `Instance` is initialized
    /// from Javascript with an already existing `WebAssembly.Instance` but with
    /// a imports from the Rust side.
    ///
    /// *This method is only available when targeting JS environments*
    pub fn init_envs(&self, imports: &[Export]) -> Result<(), InstantiationError> {
        for import in imports {
            if let Export::Function(func) = import {
                func.init_envs(&self)
                    .map_err(|e| InstantiationError::HostEnvInitialization(e))?;
            }
        }
        Ok(())
    }

    /// Gets the [`Module`] associated with this instance.
    pub fn module(&self) -> &Module {
        &self.module
    }

    /// Returns the [`Store`] where the `Instance` belongs.
    pub fn store(&self) -> &Store {
        self.module.store()
    }

    /// Returns the inner WebAssembly Instance
    #[doc(hidden)]
    pub fn raw(&self) -> &WebAssembly::Instance {
        &self.instance
    }

    #[doc(hidden)]
    pub fn resolve<'a, T, Args, Rets>(&'a self, name: &str) -> Result<T, crate::ExportError>
    where
        Args: crate::WasmTypeList,
        Rets: crate::WasmTypeList,
        T: super::exports::ExportableWithGenerics<'a, Args, Rets>,
    {
        match self.exports.get_with_generics_weak(name) {
            Ok(a) => Ok(a),
            Err(crate::ExportError::Missing(a)) if a == "memory" => {
                for (_, _, import) in self.imports.iter() {
                    if let Extern::Memory(_) = import {
                        return T::get_self_from_extern_with_generics(import);
                    }
                }
                Err(crate::ExportError::Missing("memory".to_string()))
            }
            Err(err) => Err(err)
        }
    }

    /// Creates a a thread and returns it
    pub fn new_thread(&self) -> ThreadControl {
        let id = self.thread_seed.fetch_add(1, Ordering::AcqRel) + 1;
        ThreadControl::new(id)
    }
}

impl fmt::Debug for Instance {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        f.debug_struct("Instance")
            .field("exports", &self.exports)
            .finish()
    }
}
