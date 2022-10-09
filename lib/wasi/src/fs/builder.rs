use std::path::Path;
use tracing::*;
use wasmer_vfs::FileSystem;

use super::{UnionFileSystem, TmpFileSystem};

pub struct RootFileSystemBuilder
{
    default_root_dirs: bool,
}

impl RootFileSystemBuilder
{
    pub fn new() -> Self {
        Self {
            default_root_dirs: true
        }
    }

    pub fn default_root_dirs(mut self, val: bool) -> Self {
        self.default_root_dirs = val;
        self
    }

    pub fn build(self) -> UnionFileSystem {
        let tmp = TmpFileSystem::new();
        if self.default_root_dirs {
            for root_dir in vec![
                "/.app",
                "/.private",
                "/bin",
                "/dev",
                "/etc",
                "/tmp"
            ] {
                if let Err(err) = tmp.create_dir(&Path::new(root_dir)) {
                    debug!("failed to create dir [{}] - {}", root_dir, err);
                }
            }
        }        
        let mut union = UnionFileSystem::new();
        union.mount("/", "/", false, Box::new(tmp), None);
        union
    }
}