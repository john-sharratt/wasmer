//! Builder system for configuring a [`WasiState`] and creating it.

#[cfg(feature = "os")]
use crate::bin_factory::CachedCompiledModules;
#[cfg(feature = "os")]
use crate::fs::RootFileSystemBuilder;
use crate::fs::{TtyFile, ArcFile};
use crate::state::{default_fs_backing, WasiFs, WasiState};
use crate::syscalls::types::{__WASI_STDERR_FILENO, __WASI_STDIN_FILENO, __WASI_STDOUT_FILENO};
use crate::{WasiEnv, WasiFunctionEnv, WasiInodes, WasiControlPlane, PluggableRuntimeImplementation};
use generational_arena::Arena;
use rand::Rng;
use std::collections::HashMap;
use std::ops::{Deref, DerefMut};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::RwLock;
use thiserror::Error;
use wasmer::AsStoreMut;
use wasmer_vfs::{FsError, VirtualFile, FileSystem};

/// Creates an empty [`WasiStateBuilder`].
///
/// Internal method only, users should call [`WasiState::new`].
pub(crate) fn create_wasi_state(program_name: &str) -> WasiStateBuilder {
    WasiStateBuilder {
        args: vec![program_name.to_string()],
        ..WasiStateBuilder::default()
    }
}

/// Convenient builder API for configuring WASI via [`WasiState`].
///
/// Usage:
/// ```no_run
/// # use wasmer_wasi::{WasiState, WasiStateCreationError};
/// # fn main() -> Result<(), WasiStateCreationError> {
/// let mut state_builder = WasiState::new("wasi-prog-name");
/// state_builder
///    .env("ENV_VAR", "ENV_VAL")
///    .arg("--verbose")
///    .preopen_dir("src")?
///    .map_dir("name_wasi_sees", "path/on/host/fs")?
///    .build();
/// # Ok(())
/// # }
/// ```
#[derive(Default)]
pub struct WasiStateBuilder {
    args: Vec<String>,
    envs: Vec<(String, Vec<u8>)>,
    preopens: Vec<PreopenedDir>,
    uses: Vec<String>,
    #[cfg(feature = "sys")]
    map_commands: HashMap<String, PathBuf>,
    vfs_preopens: Vec<String>,
    full_sandbox: bool,
    #[cfg(feature = "os")]
    compiled_modules: Arc<CachedCompiledModules>,
    #[allow(clippy::type_complexity)]
    setup_fs_fn: Option<Box<dyn Fn(&mut WasiInodes, &mut WasiFs) -> Result<(), String> + Send>>,
    stdout_override: Option<Box<dyn VirtualFile + Send + Sync + 'static>>,
    stderr_override: Option<Box<dyn VirtualFile + Send + Sync + 'static>>,
    stdin_override: Option<Box<dyn VirtualFile + Send + Sync + 'static>>,
    fs_override: Option<Box<dyn wasmer_vfs::FileSystem + Send + Sync>>,
    runtime_override: Option<Arc<dyn crate::WasiRuntimeImplementation + Send + Sync + 'static>>,
}

impl std::fmt::Debug for WasiStateBuilder {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // TODO: update this when stable
        f.debug_struct("WasiStateBuilder")
            .field("args", &self.args)
            .field("envs", &self.envs)
            .field("preopens", &self.preopens)
            .field("uses", &self.uses)
            .field("setup_fs_fn exists", &self.setup_fs_fn.is_some())
            .field("stdout_override exists", &self.stdout_override.is_some())
            .field("stderr_override exists", &self.stderr_override.is_some())
            .field("stdin_override exists", &self.stdin_override.is_some())
            .field("runtime_override_exists", &self.runtime_override.is_some())
            .finish()
    }
}

/// Error type returned when bad data is given to [`WasiStateBuilder`].
#[derive(Error, Debug, PartialEq, Eq)]
pub enum WasiStateCreationError {
    #[error("bad environment variable format: `{0}`")]
    EnvironmentVariableFormatError(String),
    #[error("argument contains null byte: `{0}`")]
    ArgumentContainsNulByte(String),
    #[error("preopened directory not found: `{0}`")]
    PreopenedDirectoryNotFound(PathBuf),
    #[error("preopened directory error: `{0}`")]
    PreopenedDirectoryError(String),
    #[error("mapped dir alias has wrong format: `{0}`")]
    MappedDirAliasFormattingError(String),
    #[error("wasi filesystem creation error: `{0}`")]
    WasiFsCreationError(String),
    #[error("wasi filesystem setup error: `{0}`")]
    WasiFsSetupError(String),
    #[error(transparent)]
    FileSystemError(FsError),
    #[error("wasi inherit error: `{0}`")]
    WasiInheritError(String),
}

fn validate_mapped_dir_alias(alias: &str) -> Result<(), WasiStateCreationError> {
    if !alias.bytes().all(|b| b != b'\0') {
        return Err(WasiStateCreationError::MappedDirAliasFormattingError(
            format!("Alias \"{}\" contains a nul byte", alias),
        ));
    }

    Ok(())
}

pub type SetupFsFn = Box<dyn Fn(&mut WasiInodes, &mut WasiFs) -> Result<(), String> + Send>;

// TODO add other WasiFS APIs here like swapping out stdout, for example (though we need to
// return stdout somehow, it's unclear what that API should look like)
impl WasiStateBuilder {
    /// Add an environment variable pair.
    ///
    /// Both the key and value of an environment variable must not
    /// contain a nul byte (`0x0`), and the key must not contain the
    /// `=` byte (`0x3d`).
    pub fn env<Key, Value>(&mut self, key: Key, value: Value) -> &mut Self
    where
        Key: AsRef<[u8]>,
        Value: AsRef<[u8]>,
    {
        self.envs
            .push((String::from_utf8_lossy(key.as_ref()).to_string(), value.as_ref().to_vec()));

        self
    }

    /// Add an argument.
    ///
    /// Arguments must not contain the nul (0x0) byte
    pub fn arg<Arg>(&mut self, arg: Arg) -> &mut Self
    where
        Arg: AsRef<[u8]>,
    {
        self.args.push(String::from_utf8_lossy(arg.as_ref()).to_string());

        self
    }

    /// Add multiple environment variable pairs.
    ///
    /// Both the key and value of the environment variables must not
    /// contain a nul byte (`0x0`), and the key must not contain the
    /// `=` byte (`0x3d`).
    pub fn envs<I, Key, Value>(&mut self, env_pairs: I) -> &mut Self
    where
        I: IntoIterator<Item = (Key, Value)>,
        Key: AsRef<[u8]>,
        Value: AsRef<[u8]>,
    {
        env_pairs.into_iter().for_each(|(key, value)| {
            self.env(key, value);
        });

        self
    }

    /// Adds a container this module inherits from
    pub fn use_webc<Name>(&mut self, webc: Name) -> &mut Self
    where
        Name: AsRef<str>,
    {
        self.uses.push(webc.as_ref().to_string());
        self
    }

    /// Adds a list of other containers this module inherits from
    pub fn uses<I>(&mut self, uses: I) -> &mut Self
    where
        I: IntoIterator<Item = String>,
    {
        uses.into_iter().for_each(|inherit| {
            self.uses.push(inherit);
        });
        self
    }

    /// Map an atom to a local binary
    #[cfg(feature = "sys")]
    pub fn map_command<Name, Target>(&mut self, name: Name, target: Target) -> &mut Self
    where
        Name: AsRef<str>,
        Target: AsRef<str>,
    {
        let path_buf = PathBuf::from(target.as_ref().to_string());
        self.map_commands.insert(name.as_ref().to_string(), path_buf);
        self
    }

    /// Maps a series of atoms to the local binaries
    #[cfg(feature = "sys")]
    pub fn map_commands<I, Name, Target>(&mut self, map_commands: I) -> &mut Self
    where
        I: IntoIterator<Item = (Name, Target)>,
        Name: AsRef<str>,
        Target: AsRef<str>,
    {
        map_commands.into_iter().for_each(|(name, target)| {
            let path_buf = PathBuf::from(target.as_ref().to_string());
            self.map_commands.insert(name.as_ref().to_string(), path_buf);
        });
        self
    }

    /// Add multiple arguments.
    ///
    /// Arguments must not contain the nul (0x0) byte
    pub fn args<I, Arg>(&mut self, args: I) -> &mut Self
    where
        I: IntoIterator<Item = Arg>,
        Arg: AsRef<[u8]>,
    {
        args.into_iter().for_each(|arg| {
            self.arg(arg);
        });

        self
    }

    /// Preopen a directory
    ///
    /// This opens the given directory at the virtual root, `/`, and allows
    /// the WASI module to read and write to the given directory.
    pub fn preopen_dir<FilePath>(
        &mut self,
        po_dir: FilePath,
    ) -> Result<&mut Self, WasiStateCreationError>
    where
        FilePath: AsRef<Path>,
    {
        let mut pdb = PreopenDirBuilder::new();
        let path = po_dir.as_ref();
        pdb.directory(path).read(true).write(true).create(true);
        let preopen = pdb.build()?;

        self.preopens.push(preopen);

        Ok(self)
    }

    /// Preopen a directory and configure it.
    ///
    /// Usage:
    ///
    /// ```no_run
    /// # use wasmer_wasi::{WasiState, WasiStateCreationError};
    /// # fn main() -> Result<(), WasiStateCreationError> {
    /// WasiState::new("program_name")
    ///    .preopen(|p| p.directory("src").read(true).write(true).create(true))?
    ///    .preopen(|p| p.directory(".").alias("dot").read(true))?
    ///    .build()?;
    /// # Ok(())
    /// # }
    /// ```
    pub fn preopen<F>(&mut self, inner: F) -> Result<&mut Self, WasiStateCreationError>
    where
        F: Fn(&mut PreopenDirBuilder) -> &mut PreopenDirBuilder,
    {
        let mut pdb = PreopenDirBuilder::new();
        let po_dir = inner(&mut pdb).build()?;

        self.preopens.push(po_dir);

        Ok(self)
    }

    /// Preopen a directory.
    ///
    /// This opens the given directory at the virtual root, `/`, and allows
    /// the WASI module to read and write to the given directory.
    pub fn preopen_dirs<I, FilePath>(
        &mut self,
        po_dirs: I,
    ) -> Result<&mut Self, WasiStateCreationError>
    where
        I: IntoIterator<Item = FilePath>,
        FilePath: AsRef<Path>,
    {
        for po_dir in po_dirs {
            self.preopen_dir(po_dir)?;
        }

        Ok(self)
    }

    /// Preopen the given directories from the
    /// Virtual FS.
    pub fn preopen_vfs_dirs<I>(&mut self, po_dirs: I) -> Result<&mut Self, WasiStateCreationError>
    where
        I: IntoIterator<Item = String>,
    {
        for po_dir in po_dirs {
            self.vfs_preopens.push(po_dir);
        }

        Ok(self)
    }

    /// Preopen a directory with a different name exposed to the WASI.
    pub fn map_dir<FilePath>(
        &mut self,
        alias: &str,
        po_dir: FilePath,
    ) -> Result<&mut Self, WasiStateCreationError>
    where
        FilePath: AsRef<Path>,
    {
        let mut pdb = PreopenDirBuilder::new();
        let path = po_dir.as_ref();
        pdb.directory(path)
            .alias(alias)
            .read(true)
            .write(true)
            .create(true);
        let preopen = pdb.build()?;

        self.preopens.push(preopen);

        Ok(self)
    }

    /// Preopen directorys with a different names exposed to the WASI.
    pub fn map_dirs<I, FilePath>(
        &mut self,
        mapped_dirs: I,
    ) -> Result<&mut Self, WasiStateCreationError>
    where
        I: IntoIterator<Item = (String, FilePath)>,
        FilePath: AsRef<Path>,
    {
        for (alias, dir) in mapped_dirs {
            self.map_dir(&alias, dir)?;
        }

        Ok(self)
    }

    /// Sets if this state will use a full sandbox
    pub fn full_sandbox(&mut self) -> &mut Self {
        self.full_sandbox = true;
        self
    }

    /// Overwrite the default WASI `stdout`, if you want to hold on to the
    /// original `stdout` use [`WasiFs::swap_file`] after building.
    pub fn stdout(&mut self, new_file: Box<dyn VirtualFile + Send + Sync + 'static>) -> &mut Self {
        self.stdout_override = Some(new_file);

        self
    }

    /// Overwrite the default WASI `stderr`, if you want to hold on to the
    /// original `stderr` use [`WasiFs::swap_file`] after building.
    pub fn stderr(&mut self, new_file: Box<dyn VirtualFile + Send + Sync + 'static>) -> &mut Self {
        self.stderr_override = Some(new_file);

        self
    }

    /// Overwrite the default WASI `stdin`, if you want to hold on to the
    /// original `stdin` use [`WasiFs::swap_file`] after building.
    pub fn stdin(&mut self, new_file: Box<dyn VirtualFile + Send + Sync + 'static>) -> &mut Self {
        self.stdin_override = Some(new_file);

        self
    }

    /// Sets the FileSystem to be used with this WASI instance.
    ///
    /// This is usually used in case a custom `wasmer_vfs::FileSystem` is needed.
    pub fn set_fs(&mut self, fs: Box<dyn wasmer_vfs::FileSystem + Send + Sync>) -> &mut Self {
        self.fs_override = Some(fs);

        self
    }

    /// Configure the WASI filesystem before running.
    // TODO: improve ergonomics on this function
    pub fn setup_fs(&mut self, setup_fs_fn: SetupFsFn) -> &mut Self {
        self.setup_fs_fn = Some(setup_fs_fn);

        self
    }

    /// Sets the WASI runtime implementation and overrides the default
    /// implementation
    pub fn runtime<R>(&mut self, runtime: R) -> &mut Self
    where
        R: crate::WasiRuntimeImplementation + Send + Sync + 'static,
    {
        self.runtime_override = Some(Arc::new(runtime));
        self
    }

    /// Sets the compiled modules to use with this builder (sharing the
    /// cached modules is better for performance and memory consumption)
    #[cfg(feature = "os")]
    pub fn compiled_modules(&mut self, compiled_modules: &Arc<CachedCompiledModules>) -> &mut Self {
        let mut compiled_modules = compiled_modules.clone();
        std::mem::swap(&mut self.compiled_modules, &mut compiled_modules);
        self
    }

    /// Consumes the [`WasiStateBuilder`] and produces a [`WasiState`]
    ///
    /// Returns the error from `WasiFs::new` if there's an error
    ///
    /// # Calling `build` multiple times
    ///
    /// Calling this method multiple times might not produce a
    /// determinisic result. This method is changing the builder's
    /// internal state. The values set with the following methods are
    /// reset to their defaults:
    ///
    /// * [Self::set_fs],
    /// * [Self::stdin],
    /// * [Self::stdout],
    /// * [Self::stderr].
    ///
    /// Ideally, the builder must be refactord to update `&mut self`
    /// to `mut self` for every _builder method_, but it will break
    /// existing code. It will be addressed in a next major release.
    pub fn build(&mut self) -> Result<WasiState, WasiStateCreationError> {
        for arg in self.args.iter() {
            for b in arg.as_bytes().iter() {
                if *b == 0 {
                    return Err(WasiStateCreationError::ArgumentContainsNulByte(
                        arg.clone(),
                    ));
                }
            }
        }

        enum InvalidCharacter {
            Nul,
            Equal,
        }

        for (env_key, env_value) in self.envs.iter() {
            match env_key.as_bytes().iter().find_map(|&ch| {
                if ch == 0 {
                    Some(InvalidCharacter::Nul)
                } else if ch == b'=' {
                    Some(InvalidCharacter::Equal)
                } else {
                    None
                }
            }) {
                Some(InvalidCharacter::Nul) => {
                    return Err(WasiStateCreationError::EnvironmentVariableFormatError(
                        format!(
                            "found nul byte in env var key \"{}\" (key=value)",
                            env_key
                        ),
                    ))
                }

                Some(InvalidCharacter::Equal) => {
                    return Err(WasiStateCreationError::EnvironmentVariableFormatError(
                        format!(
                            "found equal sign in env var key \"{}\" (key=value)",
                            env_key
                        ),
                    ))
                }

                None => (),
            }

            if env_value.iter().any(|&ch| ch == 0) {
                return Err(WasiStateCreationError::EnvironmentVariableFormatError(
                    format!(
                        "found nul byte in env var value \"{}\" (key=value)",
                        String::from_utf8_lossy(env_value),
                    ),
                ));
            }
        }

        // Get a reference to the runtime        
        let runtime = self.runtime_override.clone().unwrap_or_else( || {
            Arc::new(PluggableRuntimeImplementation::default())
        });

        // Determine the STDIN
        let stdin: Box<ArcFile> = self.stdin_override
            .take()
            .map(|a| Box::new(ArcFile::new(a)))
            .unwrap_or_else(|| {
                Box::new(ArcFile::new(Box::new(super::Stdin::default())))
            });

        // If we are running WASIX then we start a full sandbox FS
        // otherwise we drop through to a default file system
        let fs_backing = self.fs_override
            .take()
            .unwrap_or_else(|| -> Box<dyn FileSystem + Send + Sync> {
                #[cfg(feature = "os")]
                if self.full_sandbox {
                    RootFileSystemBuilder::new()
                        .with_tty(Box::new(TtyFile::new(runtime.clone(), stdin.clone())))
                        .build()
                } else {
                    default_fs_backing()
                }
                #[cfg(not(feature = "os"))]
                default_fs_backing()
            });

        // self.preopens are checked in [`PreopenDirBuilder::build`]
        let inodes = RwLock::new(crate::state::WasiInodes {
            arena: Arena::new(),
            orphan_fds: HashMap::new(),
        });
        let wasi_fs = {
            let mut inodes = inodes.write().unwrap();

            // self.preopens are checked in [`PreopenDirBuilder::build`]
            let mut wasi_fs = WasiFs::new_with_preopen(
                inodes.deref_mut(),
                &self.preopens,
                &self.vfs_preopens,
                fs_backing,
            )
            .map_err(WasiStateCreationError::WasiFsCreationError)?;

            // set up the file system, overriding base files and calling the setup function
            wasi_fs
                .swap_file(inodes.deref(), __WASI_STDIN_FILENO, stdin)
                .map_err(WasiStateCreationError::FileSystemError)?;

            if let Some(stdout_override) = self.stdout_override.take() {
                wasi_fs
                    .swap_file(inodes.deref(), __WASI_STDOUT_FILENO, stdout_override)
                    .map_err(WasiStateCreationError::FileSystemError)?;
            }

            if let Some(stderr_override) = self.stderr_override.take() {
                wasi_fs
                    .swap_file(inodes.deref(), __WASI_STDERR_FILENO, stderr_override)
                    .map_err(WasiStateCreationError::FileSystemError)?;
            }

            if let Some(f) = &self.setup_fs_fn {
                f(inodes.deref_mut(), &mut wasi_fs)
                    .map_err(WasiStateCreationError::WasiFsSetupError)?;
            }
            wasi_fs
        };
        let inodes = Arc::new(inodes);

        Ok(WasiState {
            fs: wasi_fs,
            secret: rand::thread_rng().gen::<[u8; 32]>(),
            inodes,
            args: self.args.clone(),
            preopen: self.vfs_preopens.clone(),
            threading: Default::default(),
            futexs: Default::default(),
            clock_offset: Default::default(),
            bus: Default::default(),
            runtime,
            envs: self
                .envs
                .iter()
                .map(|(key, value)| {
                    let mut env = Vec::with_capacity(key.len() + value.len() + 1);
                    env.extend_from_slice(key.as_bytes());
                    env.push(b'=');
                    env.extend_from_slice(value);

                    env
                })
                .collect(),
        })
    }

    /// Consumes the [`WasiStateBuilder`] and produces a [`WasiEnv`]
    ///
    /// Returns the error from `WasiFs::new` if there's an error.
    ///
    /// # Calling `finalize` multiple times
    ///
    /// Calling this method multiple times might not produce a
    /// determinisic result. This method is calling [Self::build],
    /// which is changing the builder's internal state. See
    /// [Self::build]'s documentation to learn more.
    pub fn finalize(
        &mut self,
        store: &mut impl AsStoreMut,
    ) -> Result<WasiFunctionEnv, WasiStateCreationError> {
        let control_plane = WasiControlPlane::default();
        self.finalize_with(store, &control_plane)
    }

    /// Consumes the [`WasiStateBuilder`] and produces a [`WasiEnv`]
    /// with a particular control plane
    ///
    /// Returns the error from `WasiFs::new` if there's an error.
    ///
    /// # Calling `finalize` multiple times
    ///
    /// Calling this method multiple times might not produce a
    /// determinisic result. This method is calling [Self::build],
    /// which is changing the builder's internal state. See
    /// [Self::build]'s documentation to learn more.
    pub fn finalize_with(
        &mut self,
        store: &mut impl AsStoreMut,
        control_plane: &WasiControlPlane,
    ) -> Result<WasiFunctionEnv, WasiStateCreationError> {
        let state = Arc::new(self.build()?);
        let runtime = state.runtime.clone();

        let process = control_plane.new_process();
        let thread = process.new_thread();

        let env = WasiEnv::new_ext(
            state,
            #[cfg(feature = "os")]
            self.compiled_modules.clone(),
            process,
            thread,
            runtime
        );

        env.uses(self.uses.clone())?;
        #[cfg(feature = "sys")]
        env.map_commands(self.map_commands.clone())?;

        Ok(WasiFunctionEnv::new(store, env))
    }
}

/// Builder for preopened directories.
#[derive(Debug, Default)]
pub struct PreopenDirBuilder {
    path: Option<PathBuf>,
    alias: Option<String>,
    read: bool,
    write: bool,
    create: bool,
}

/// The built version of `PreopenDirBuilder`
#[derive(Debug, Clone, Default)]
pub(crate) struct PreopenedDir {
    pub(crate) path: PathBuf,
    pub(crate) alias: Option<String>,
    pub(crate) read: bool,
    pub(crate) write: bool,
    pub(crate) create: bool,
}

impl PreopenDirBuilder {
    /// Create an empty builder
    pub(crate) fn new() -> Self {
        PreopenDirBuilder::default()
    }

    /// Point the preopened directory to the path given by `po_dir`
    pub fn directory<FilePath>(&mut self, po_dir: FilePath) -> &mut Self
    where
        FilePath: AsRef<Path>,
    {
        let path = po_dir.as_ref();
        self.path = Some(path.to_path_buf());

        self
    }

    /// Make this preopened directory appear to the WASI program as `alias`
    pub fn alias(&mut self, alias: &str) -> &mut Self {
        // We mount at preopened dirs at `/` by default and multiple `/` in a row
        // are equal to a single `/`.
        let alias = alias.trim_start_matches('/');
        self.alias = Some(alias.to_string());

        self
    }

    /// Set read permissions affecting files in the directory
    pub fn read(&mut self, toggle: bool) -> &mut Self {
        self.read = toggle;

        self
    }

    /// Set write permissions affecting files in the directory
    pub fn write(&mut self, toggle: bool) -> &mut Self {
        self.write = toggle;

        self
    }

    /// Set create permissions affecting files in the directory
    ///
    /// Create implies `write` permissions
    pub fn create(&mut self, toggle: bool) -> &mut Self {
        self.create = toggle;
        if toggle {
            self.write = true;
        }

        self
    }

    pub(crate) fn build(&self) -> Result<PreopenedDir, WasiStateCreationError> {
        // ensure at least one is set
        if !(self.read || self.write || self.create) {
            return Err(WasiStateCreationError::PreopenedDirectoryError("Preopened directories must have at least one of read, write, create permissions set".to_string()));
        }

        if self.path.is_none() {
            return Err(WasiStateCreationError::PreopenedDirectoryError(
                "Preopened directories must point to a host directory".to_string(),
            ));
        }
        let path = self.path.clone().unwrap();

        /*
        if !path.exists() {
            return Err(WasiStateCreationError::PreopenedDirectoryNotFound(path));
        }
        */

        if let Some(alias) = &self.alias {
            validate_mapped_dir_alias(alias)?;
        }

        Ok(PreopenedDir {
            path,
            alias: self.alias.clone(),
            read: self.read,
            write: self.write,
            create: self.create,
        })
    }
}

#[cfg(test)]
mod test {
    use super::*;

    #[test]
    fn env_var_errors() {
        // `=` in the key is invalid.
        assert!(
            create_wasi_state("test_prog")
                .env("HOM=E", "/home/home")
                .build()
                .is_err(),
            "equal sign in key must be invalid"
        );

        // `\0` in the key is invalid.
        assert!(
            create_wasi_state("test_prog")
                .env("HOME\0", "/home/home")
                .build()
                .is_err(),
            "nul in key must be invalid"
        );

        // `=` in the value is valid.
        assert!(
            create_wasi_state("test_prog")
                .env("HOME", "/home/home=home")
                .build()
                .is_ok(),
            "equal sign in the value must be valid"
        );

        // `\0` in the value is invalid.
        assert!(
            create_wasi_state("test_prog")
                .env("HOME", "/home/home\0")
                .build()
                .is_err(),
            "nul in value must be invalid"
        );
    }

    #[test]
    fn nul_character_in_args() {
        let output = create_wasi_state("test_prog").arg("--h\0elp").build();
        match output {
            Err(WasiStateCreationError::ArgumentContainsNulByte(_)) => assert!(true),
            _ => assert!(false),
        }
        let output = create_wasi_state("test_prog")
            .args(&["--help", "--wat\0"])
            .build();
        match output {
            Err(WasiStateCreationError::ArgumentContainsNulByte(_)) => assert!(true),
            _ => assert!(false),
        }
    }
}
