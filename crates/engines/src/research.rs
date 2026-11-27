//! Public reference retrieval with bounded responses and a shared seven-day cache.

use crate::{CancellationToken, EngineError, EngineService, adaptation::file_error};
use gamel10n_translation::files::{hash_file, hash_text, read_json, write_json};
use serde::{Deserialize, Serialize};
use std::{
    fs,
    io::Write,
    net::IpAddr,
    path::{Path, PathBuf},
    time::Duration,
};
use tokio_util::task::AbortOnDropHandle;

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ResearchDocument {
    pub url: String,
    pub content: Vec<u8>,
    pub retrieved_at: chrono::DateTime<chrono::Utc>,
    pub sha256: String,
    pub reused: bool,
    pub stale: bool,
    pub media_type: Option<String>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct DownloadReceipt {
    pub url: String,
    pub sha256: String,
    pub bytes: u64,
    pub retrieved_at: chrono::DateTime<chrono::Utc>,
    pub media_type: Option<String>,
}

pub(crate) struct PublicDownload {
    pub path: PathBuf,
    pub source: DownloadReceipt,
    pub reused: bool,
    pub stale: bool,
}

fn cached_download(
    directory: &Path,
    url: &str,
    expected: Option<&str>,
    maximum_bytes: u64,
) -> Result<Option<PublicDownload>, EngineError> {
    let receipt_path = directory.join(format!("{}.json", expected.unwrap_or("source")));
    if !receipt_path.is_file() || receipt_path.metadata()?.len() > 32768 {
        return Ok(None);
    }
    let Ok(receipt) = read_json::<DownloadReceipt>(&receipt_path) else {
        return Ok(None);
    };
    if receipt.url != url
        || receipt.sha256.len() != 64
        || !receipt.sha256.bytes().all(|byte| byte.is_ascii_hexdigit())
        || expected.is_some_and(|expected| expected != receipt.sha256)
        || receipt.retrieved_at <= chrono::DateTime::UNIX_EPOCH
        || receipt.retrieved_at > chrono::Utc::now() + chrono::Duration::minutes(5)
    {
        return Ok(None);
    }
    if receipt.bytes > maximum_bytes {
        return Err(EngineError::Rejected(format!(
            "公开文件超过 {} KiB 读取上限，请选择必要的工具文件或具体资料；未将截断内容当作完整资料。",
            maximum_bytes / 1024
        )));
    }
    let path = directory.join(format!("{}.bin", receipt.sha256));
    if !path.is_file()
        || path.metadata()?.len() != receipt.bytes
        || hash_file(&path).map_err(file_error)? != receipt.sha256
    {
        return Ok(None);
    }
    Ok(Some(PublicDownload {
        path,
        source: receipt,
        reused: true,
        stale: false,
    }))
}

impl EngineService {
    pub(crate) fn has_verified_public_download(
        &self,
        url: &str,
        sha256: &str,
    ) -> Result<bool, EngineError> {
        let Ok(parsed) = reqwest::Url::parse(url) else {
            return Ok(false);
        };
        if validate_url(&parsed).is_err()
            || sha256.len() != 64
            || !sha256.bytes().all(|byte| byte.is_ascii_hexdigit())
        {
            return Ok(false);
        }
        let directory = self
            .shared_directory
            .join("downloads")
            .join(public_download_cache_key(&parsed));
        // Workspace provenance is editable; only the host's verified download is evidence.
        Ok(
            cached_download(&directory, parsed.as_str(), Some(sha256), 256 * 1024 * 1024)?
                .is_some(),
        )
    }

    pub(crate) async fn fetch_public_download(
        &self,
        url: &str,
        refresh: bool,
        expected: Option<&str>,
        cancellation: &CancellationToken,
    ) -> Result<PublicDownload, EngineError> {
        self.read_public_download(
            url,
            refresh,
            expected,
            256 * 1024 * 1024,
            None,
            cancellation,
        )
        .await
    }

    async fn read_public_download(
        &self,
        url: &str,
        refresh: bool,
        expected: Option<&str>,
        maximum_bytes: u64,
        maximum_age: Option<chrono::Duration>,
        cancellation: &CancellationToken,
    ) -> Result<PublicDownload, EngineError> {
        if cancellation.is_cancelled() {
            return Err(EngineError::Cancelled);
        }
        let parsed =
            reqwest::Url::parse(url).map_err(|_| EngineError::Protocol("资料地址无效。".into()))?;
        validate_url(&parsed)?;
        if expected.is_some_and(|hash| {
            hash.len() != 64 || !hash.bytes().all(|byte| byte.is_ascii_hexdigit())
        }) {
            return Err(EngineError::Protocol(
                "已登记工具的 SHA256 无效，不能据此恢复固定版本。".into(),
            ));
        }
        let canonical = parsed.to_string();
        let directory = self
            .shared_directory
            .join("downloads")
            .join(public_download_cache_key(&parsed));
        let cached = match if refresh {
            None
        } else {
            cached_download(&directory, &canonical, expected, maximum_bytes)?
        } {
            Some(cached)
                if maximum_age.is_none_or(|maximum_age| {
                    chrono::Utc::now() - cached.source.retrieved_at <= maximum_age
                }) =>
            {
                return Ok(cached);
            }
            cached => cached,
        };
        let request = async {
            // Debug HTTPS proxy connections need a shallow call stack.
            let mut response = AbortOnDropHandle::new(tokio::spawn(public_response(parsed)))
                .await
                .map_err(|error| {
                    EngineError::Research(format!("公开资料请求任务中断（{error}）"))
                })??;
            if response
                .content_length()
                .is_some_and(|bytes| bytes > maximum_bytes)
            {
                return Err(EngineError::Rejected(format!(
                    "公开文件超过 {} KiB 读取上限，请选择必要的工具文件或具体资料；未将截断内容当作完整资料。",
                    maximum_bytes / 1024
                )));
            }
            fs::create_dir_all(&directory)?;
            let mut temporary = tempfile::NamedTempFile::new_in(&directory)?;
            let mut bytes = 0_u64;
            while let Some(chunk) = response
                .chunk()
                .await
                .map_err(|error| EngineError::Research(error.to_string()))?
            {
                bytes += chunk.len() as u64;
                if bytes > maximum_bytes {
                    return Err(EngineError::Rejected(format!(
                        "公开文件超过 {} KiB 读取上限，请选择必要的工具文件或具体资料；未将截断内容当作完整资料。",
                        maximum_bytes / 1024
                    )));
                }
                temporary.write_all(&chunk)?;
            }
            temporary.flush()?;
            let digest = hash_file(temporary.path()).map_err(file_error)?;
            if expected.is_some_and(|expected| expected != digest) {
                return Err(EngineError::Rejected("上游文件与此项目已登记的工具版本不同，未替换固定依赖。请恢复原文件；确需更新时用 refresh=true。".into()));
            }
            let receipt = DownloadReceipt {
                url: canonical,
                sha256: digest.clone(),
                bytes,
                retrieved_at: chrono::Utc::now(),
                media_type: response
                    .headers()
                    .get(reqwest::header::CONTENT_TYPE)
                    .and_then(|value| value.to_str().ok())
                    .map(|value| value.split(';').next().unwrap_or(value).into()),
            };
            let path = directory.join(format!("{digest}.bin"));
            if cancellation.is_cancelled() {
                return Err(EngineError::Cancelled);
            }
            temporary
                .persist(&path)
                .map_err(|error| EngineError::Io(error.error))?;
            write_json(&directory.join(format!("{digest}.json")), &receipt).map_err(file_error)?;
            write_json(&directory.join("source.json"), &receipt).map_err(file_error)?;
            Ok(PublicDownload {
                path,
                source: receipt,
                reused: false,
                stale: false,
            })
        };
        let outcome = tokio::select! {()=cancellation.cancelled()=>return Err(EngineError::Cancelled),result=request=>result};
        if cancellation.is_cancelled() {
            return Err(EngineError::Cancelled);
        }
        match (outcome, cached) {
            (Err(EngineError::Research(_)), Some(cached)) => Ok(PublicDownload {
                stale: true,
                ..cached
            }),
            (result, _) => result,
        }
    }

    pub(crate) async fn fetch_reference(
        &self,
        url: &str,
        refresh: bool,
        cancellation: &CancellationToken,
    ) -> Result<ResearchDocument, EngineError> {
        let download = self
            .read_public_download(
                url,
                refresh,
                None,
                8 * 1024 * 1024,
                Some(chrono::Duration::days(7)),
                cancellation,
            )
            .await?;
        Ok(ResearchDocument {
            url: url.into(),
            content: fs::read(&download.path)?,
            retrieved_at: download.source.retrieved_at,
            sha256: download.source.sha256,
            reused: download.reused,
            stale: download.stale,
            media_type: download.source.media_type,
        })
    }
}

fn shared_public_client() -> Result<reqwest::Client, EngineError> {
    static CLIENT: std::sync::OnceLock<Result<reqwest::Client, String>> =
        std::sync::OnceLock::new();
    CLIENT
        .get_or_init(|| {
            reqwest::Client::builder()
                .redirect(reqwest::redirect::Policy::none())
                .timeout(Duration::from_secs(180))
                .build()
                .map_err(|error| error.to_string())
        })
        .clone()
        .map_err(EngineError::Research)
}

fn public_accept_header(url: &reqwest::Url) -> &'static str {
    if github_git_repository(url, "blobs").is_some() {
        "application/vnd.github.raw+json"
    } else if url
        .host_str()
        .is_some_and(|host| host.eq_ignore_ascii_case("api.github.com"))
    {
        "application/json"
    } else {
        "*/*"
    }
}

async fn public_response(mut current: reqwest::Url) -> Result<reqwest::Response, EngineError> {
    let client = shared_public_client()?;
    for _ in 0..6 {
        validate_url(&current)?;
        let accept = public_accept_header(&current);
        let response = client
            .get(current.clone())
            .header("User-Agent", "GameL10n/1.0")
            .header(reqwest::header::ACCEPT, accept)
            .send()
            .await
            .map_err(|error| EngineError::Research(format!("公开资料请求失败：{error}")))?;
        if response.status().is_redirection()
            && let Some(location) = response
                .headers()
                .get(reqwest::header::LOCATION)
                .and_then(|value| value.to_str().ok())
        {
            current = current
                .join(location)
                .map_err(|_| EngineError::Protocol("资料重定向地址无效。".into()))?;
            continue;
        }
        if !response.status().is_success() {
            return Err(EngineError::Research(format!(
                "公开资料请求失败（HTTP {}）。",
                response.status().as_u16()
            )));
        }
        return Ok(response);
    }
    Err(EngineError::Research("资料地址重定向过多。".into()))
}

pub(crate) fn public_download_cache_key(url: &reqwest::Url) -> String {
    if github_git_repository(url, "blobs").is_some() {
        // A previous JSON/base64 response must not satisfy a raw blob request.
        hash_text(&format!("github-raw\n{url}"))
    } else {
        hash_text(url.as_str())
    }
}

pub(crate) fn github_git_repository<'a>(
    url: &'a reqwest::Url,
    object: &str,
) -> Option<(&'a str, &'a str)> {
    if url.scheme() != "https" || url.host_str()? != "api.github.com" {
        return None;
    }
    let mut segments = url.path_segments()?;
    if segments.next()? != "repos" {
        return None;
    }
    let owner = segments.next().filter(|part| !part.is_empty())?;
    let repository = segments.next().filter(|part| !part.is_empty())?;
    if segments.next()? != "git" || segments.next()? != object {
        return None;
    }
    let reference = segments.next().filter(|part| !part.is_empty())?;
    if object == "blobs" {
        if segments.next().is_some()
            || reference.len() != 40
            || !reference.bytes().all(|byte| byte.is_ascii_hexdigit())
        {
            return None;
        }
    } else if segments.any(str::is_empty) {
        return None;
    }
    Some((owner, repository))
}

pub(crate) fn validate_url(url: &reqwest::Url) -> Result<(), EngineError> {
    let host = url
        .host_str()
        .unwrap_or("")
        .trim_end_matches('.')
        .to_lowercase();
    if url.scheme() != "https"
        || !url.username().is_empty()
        || url.password().is_some()
        || url.fragment().is_some()
        || host.is_empty()
        || host == "localhost"
        || host.ends_with(".localhost")
        || host.ends_with(".local")
        || host.ends_with(".internal")
        || host.ends_with(".home")
        || url.port().is_some_and(|port| port != 443)
    {
        return Err(EngineError::Rejected(
            "仅允许无认证信息的公开 HTTPS 资料地址。".into(),
        ));
    }
    if let Ok(ip) = host.trim_matches(['[', ']']).parse::<IpAddr>()
        && !public_ip(ip)
    {
        return Err(EngineError::Rejected(
            "不能读取本地、内网或特殊用途地址。".into(),
        ));
    }
    Ok(())
}

fn public_ip(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(ip) => {
            !ip.is_private()
                && !ip.is_loopback()
                && !ip.is_link_local()
                && !ip.is_broadcast()
                && !ip.is_unspecified()
                && !ip.is_multicast()
                && ip.octets()[0] != 0
                && ip.octets()[0] < 224
                && (ip.octets()[0] != 100 || !(64..128).contains(&ip.octets()[1]))
                && !ip.is_documentation()
                && ip.octets()[..3] != [192, 0, 0]
                && ip.octets()[..3] != [192, 88, 99]
                && ip.octets()[..2] != [198, 18]
                && ip.octets()[..2] != [198, 19]
        }
        IpAddr::V6(ip) => ip
            .to_ipv4_mapped()
            .map(|value| public_ip(IpAddr::V4(value)))
            .unwrap_or_else(|| {
                let [prefix, subnet, ..] = ip.segments();
                prefix & 0xe000 == 0x2000
                    && prefix != 0x2002
                    && !(prefix == 0x2001 && (subnet < 0x0200 || subnet == 0x0db8))
                    && !(prefix == 0x3fff && subnet < 0x1000)
            }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use gamel10n_translation::files::hash_bytes;
    #[tokio::test]
    async fn downloads_reuse_hash_pinned_files_above_reference_page_limit() {
        let root = tempfile::tempdir().expect("fixture");
        let assets = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
        let service = EngineService::new(&assets, crate::ToolPaths::development())
            .with_shared_directory(root.path());
        let url = "https://example.com/tool.zip";
        let directory = root.path().join("downloads").join(hash_text(url));
        fs::create_dir_all(&directory).expect("cache");
        let content = vec![42_u8; 9 * 1024 * 1024];
        let digest = hash_bytes(&content);
        fs::write(directory.join(format!("{digest}.bin")), &content).expect("cached file");
        let receipt = DownloadReceipt {
            url: url.into(),
            sha256: digest.clone(),
            bytes: content.len() as u64,
            retrieved_at: chrono::Utc::now(),
            media_type: None,
        };
        write_json(&directory.join(format!("{digest}.json")), &receipt).expect("receipt");
        let result = service
            .fetch_public_download(url, false, Some(&digest), &CancellationToken::new())
            .await
            .expect("pinned cache");
        assert!(result.reused);
        assert!(
            service
                .has_verified_public_download(url, &digest)
                .expect("verified receipt")
        );
        assert_eq!(result.source.bytes, 9 * 1024 * 1024);
        write_json(&directory.join("source.json"), &receipt).expect("URL receipt");
        assert!(
            service
                .fetch_reference(url, false, &CancellationToken::new())
                .await
                .expect_err("reference limit")
                .to_string()
                .contains("读取上限")
        );
        fs::write(&result.path, b"tampered").expect("change cache");
        assert!(
            !service
                .has_verified_public_download(url, &digest)
                .expect("tampered download")
        );
        assert!(
            cached_download(&directory, url, Some(&digest), 256 * 1024 * 1024)
                .expect("cache validation")
                .is_none()
        );
    }
    #[test]
    fn research_rejects_private_targets_and_credentials() {
        for url in [
            "http://example.com/source",
            "https://127.0.0.1/a",
            "https://[::1]/a",
            "https://user:secret@example.com/a",
            "https://host.local/a",
        ] {
            assert!(
                validate_url(&reqwest::Url::parse(url).expect("URL")).is_err(),
                "{url}"
            );
        }
    }

    #[test]
    fn accept_header_restricts_json_to_github_api_and_uses_wildcard_for_general_downloads() {
        let blob = reqwest::Url::parse(
            "https://api.github.com/repos/example/repo/git/blobs/0123456789abcdef0123456789abcdef01234567",
        )
        .expect("blob URL");
        let api = reqwest::Url::parse("https://api.github.com/repos/example/repo/releases/latest")
            .expect("API URL");
        let font = reqwest::Url::parse("https://cdn.example.com/fonts/NotoSansSC-VF.ttf")
            .expect("font URL");
        assert_eq!(
            public_accept_header(&blob),
            "application/vnd.github.raw+json"
        );
        assert_eq!(public_accept_header(&api), "application/json");
        assert_eq!(public_accept_header(&font), "*/*");
    }
}
