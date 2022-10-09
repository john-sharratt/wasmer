use serde::*;

pub const WAPM_WEBC_URL: &'static str = "https://registry.wapm.dev/graphql?query=";
pub const WAPM_WEBC_QUERY: &'static str = r#"
{
    getPackage(name: "<NAME>") {
        lastVersion {
            distribution {
                downloadUrl,
                piritaDownloadUrl
            }
        }
    }
}"#;
pub const WAPM_WEBC_QUERY_TAG: &'static str = "<NAME>";

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct WapmWebQueryGetPackageLastVersionDistribution {
    #[serde(rename = "downloadUrl")]
    pub download_url: Option<String>,
    #[serde(rename = "piritaDownloadUrl")]
    pub pirita_download_url: Option<String>,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct WapmWebQueryGetPackageLastVersion {
    #[serde(rename = "distribution")]
    pub distribution: WapmWebQueryGetPackageLastVersionDistribution
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct WapmWebQueryGetPackage {
    #[serde(rename = "lastVersion")]
    pub last_version: WapmWebQueryGetPackageLastVersion
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct WapmWebQueryData {
    #[serde(rename = "getPackage")]
    pub get_package: Option<WapmWebQueryGetPackage>
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct WapmWebQuery {
    #[serde(rename = "data")]
    pub data: WapmWebQueryData,
}