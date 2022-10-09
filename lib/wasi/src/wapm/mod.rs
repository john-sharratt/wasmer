use std::{
    sync::Arc,
    ops::Deref,
    path::PathBuf,
};
use webc::{FsEntryType, WebC, Annotation};
use webc_vfs::VirtualFileSystem;
use tracing::*;

#[allow(unused_imports)]
use tracing::{error, warn};

use crate::{
    runtime::{
        ReqwestOptions
    },
    bin_factory::{BinaryPackage, BinaryPackageCommand}, WasiRuntimeImplementation
};

mod pirita;
#[cfg(feature = "wapm-tar")]
mod manifest;

use pirita::*;

pub(crate) fn fetch_webc(cache_dir: &str, name: &str, runtime: &dyn WasiRuntimeImplementation) -> Option<BinaryPackage> {
    let url = format!(
        "{}{}",
        WAPM_WEBC_URL,
        urlencoding::encode(
            WAPM_WEBC_QUERY.replace(WAPM_WEBC_QUERY_TAG,
                name.replace("\"", "'").as_str())
            .as_str()
        )
    );
    let options = ReqwestOptions::default();
    let headers = Default::default();
    let data = None;
    match runtime.reqwest(url.as_str(), "POST", options, headers, data) {
        Ok(wapm) => {
            if wapm.status == 200 {
                if let Some(data) = wapm.data {
                    match serde_json::from_slice::<'_, WapmWebQuery>(data.as_ref()) {
                        Ok(query) => {
                            if let Some(package) = query.data.get_package {
                                if let Some(pirita_download_url) = package.last_version.distribution.pirita_download_url {
                                    return download_webc(cache_dir, name, pirita_download_url, runtime);
                                } else {
                                    warn!("package has no pirita or tar build: {}", String::from_utf8_lossy(data.as_ref()));
                                }
                            } else {
                                warn!("failed to parse WAPM package: {}", String::from_utf8_lossy(data.as_ref()));    
                            }
                        },
                        Err(err) => {
                            warn!("failed to deserialize WAPM response: {}", err);
                        }
                    }
                }
            } else {
                warn!("failed to contact WAPM: http_code={}, http_response={}", wapm.status, wapm.status_text);
            }
        },
        Err(code) => {
            warn!("failed to contact WAPM: http_code={}", code);
        }
    }
    None
}

fn download_webc(cache_dir: &str, name: &str, pirita_download_url: String, runtime: &dyn WasiRuntimeImplementation) -> Option<BinaryPackage>
{
    let compute_path = |cache_dir: &str, name: &str| {
        let name = name.replace("/", "._.");
        std::path::Path::new(cache_dir).join(format!("{}.webc", name.as_str()).as_str())
    };

    // build the parse options
    let options = webc::ParseOptions::default();

    // fast path
    let path = compute_path(cache_dir, name);
    #[cfg(feature = "sys")]
    if path.exists() {
        match webc::WebCMmap::parse(path.clone(), &options) {
            Ok(webc) => {
                unsafe {
                    let webc = Arc::new(webc);
                    return parse_webc(webc.as_webc_ref(), webc.clone());
                }
            },
            Err(err) => {
                warn!("failed to parse WebC: {}", err);
            }
        }
    }
    if let Ok(data) = std::fs::read(path) {
        match webc::WebCOwned::parse(data, &options) {
            Ok(webc) => {
                unsafe {
                    let webc = Arc::new(webc);
                    return parse_webc(webc.as_webc_ref(), webc.clone());
                }
            },
            Err(err) => {
                warn!("failed to parse WebC: {}", err);
            }
        }
    }

    // slow path
    let cache_dir = cache_dir.to_string();
    let name = name.to_string();
    if let Some(data) = download_miss(pirita_download_url.as_str(), runtime) {
        let path = compute_path(cache_dir.as_str(), name.as_str());
        let _ = std::fs::create_dir_all(path.parent().unwrap().clone());

        let mut temp_path = path.clone();
        let rand_128: u128 = rand::random();
        temp_path = PathBuf::from(format!("{}.{}.temp", temp_path.as_os_str().to_string_lossy(), rand_128));

        if let Err(err) = std::fs::write(temp_path.as_path(), &data[..]) {
            debug!("failed to write webc cache file [{}] - {}", temp_path.as_path().to_string_lossy(), err);
        }
        if let Err(err) = std::fs::rename(temp_path.as_path(), path.as_path()) {
            debug!("failed to rename webc cache file [{}] - {}", temp_path.as_path().to_string_lossy(), err);
        }

        #[cfg(feature = "sys")]
        match webc::WebCMmap::parse(path, &options) {
            Ok(webc) => {
                unsafe {
                    let webc = Arc::new(webc);
                    return parse_webc(webc.as_webc_ref(), webc.clone());
                }
            },
            Err(err) => {
                warn!("failed to parse WebC: {}", err);
            }
        }

        match webc::WebCOwned::parse(data, &options) {
            Ok(webc) => {
                unsafe {
                    let webc = Arc::new(webc);
                    return parse_webc(webc.as_webc_ref(), webc.clone());
                }
            },
            Err(err) => {
                warn!("failed to parse WebC: {}", err);
            }
        }
    }

    None
}

fn download_miss(download_url: &str, runtime: &dyn WasiRuntimeImplementation) -> Option<Vec<u8>> {
    let mut options = ReqwestOptions::default();
    options.gzip = true;

    let headers = Default::default();
    let data = None;

    match runtime.reqwest(download_url, "GET", options, headers, data) {
        Ok(wapm) => {
            if wapm.status == 200 {
                return wapm.data;
            } else {
                warn!("failed to download package: http_code={}, http_response={}", wapm.status, wapm.status_text);
            }
        },
        Err(code) => {
            warn!("failed to download package: http_code={}", code);
        }
    }
    None
}

unsafe fn parse_webc<'a, 'b, T>(webc: webc::WebC<'a>, ownership: Arc<T>) -> Option<BinaryPackage>
where T: std::fmt::Debug + Send + Sync + 'static,
      T: Deref<Target=WebC<'static>>
{
    let package_name = webc.get_package_name();
    let mut pck = webc.manifest.entrypoint
        .iter()
        .filter_map(|entry| {
            webc.manifest.commands.get(entry)
                .map(|a| (a, entry))
        })
        .filter_map(|(cmd, entry)| {
            let api = if cmd.runner.starts_with("https://webc.org/runner/emscripten") {
                "emscripten"
            } else if cmd.runner.starts_with("https://webc.org/runner/wasi") {
                "wasi"
            } else {
                warn!("unsupported runner - {}", cmd.runner);
                return None;
            };
            match webc.get_atom_name_for_command(api, entry.as_str()) {
                Ok(a) => Some(a),
                Err(err) => {
                    warn!("failed to find atom name for entry command({}) - {}", entry.as_str(), err);
                    None
                }
            }
        })
        .filter_map(|atom| {
            match webc.get_atom(&package_name, atom.as_str()) {
                Ok(a) => Some(a),
                Err(err) => {
                    warn!("failed to find atom for atom name({}) - {}", atom, err);
                    None
                }
            }
        })
        .map(|atom| {
            BinaryPackage::new_with_ownership(atom.into(), ownership.clone())
        })
        .next();

    if let Some(pck) = pck.as_mut() {
        let top_level_dirs = webc
            .get_volumes_for_package(&package_name)
            .into_iter()
            .flat_map(|volume| {
                webc.volumes
                    .get(&volume)
                    .unwrap()
                    .header
                    .top_level
                    .iter()
                    .filter(|e| e.fs_type == FsEntryType::Dir)
                    .map(|e| e.text.to_string())
            })
            .collect::<Vec<_>>();

        pck.webc_fs = Some(Arc::new(VirtualFileSystem::init(ownership.clone(), &package_name)));
        pck.webc_top_level_dirs = top_level_dirs;

        let root_package = webc.get_package_name();
        for (command, action) in webc.get_metadata().commands.iter() {
            if let Some(Annotation::Map(annotations)) = action.annotations.get("wasi") {

                let mut atom = None;
                let mut package = root_package.clone();
                for (k, v) in annotations {
                    match (k, v) {
                        (Annotation::Text(k), Annotation::Text(v)) if k == "atom" => {
                            atom = Some(v.clone());
                        },
                        (Annotation::Text(k), Annotation::Text(v)) if k == "package" => {
                            package = v.clone();
                        },
                        _ => { }
                    }
                }
                
                // Load the atom as a command
                if let Some(atom_name) = atom {
                    match webc.get_atom(package.as_str(), atom_name.as_str()) {
                        Ok(atom) => {
                            trace!("added atom (name={}, size={}) for command [{}]", atom_name, atom.len(), command);
                            let mut commands = pck.commands.write().unwrap();
                            commands.push(
                                BinaryPackageCommand::new_with_ownership(
                                    command.clone(),
                                    atom.into(),
                                    ownership.clone()
                                )
                            );
                        }
                        Err(err) => {
                            warn!("Failed to find atom [{}].[{}] - {}", package, atom_name, err);
                        }
                    }
                }
            }
        }
    }

    pck
}
