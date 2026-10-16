/// Public addresses are independent of the socket used behind a reverse proxy.
#[derive(Debug, Clone)]
pub struct PublicUrl {
    pub base_url: String,
    pub resource_url: String,
    pub mcp_path: String,
    pub metadata_path: String,
    pub health_path: String,
    pub origin: String,
    pub authority: String,
}

pub(crate) fn validate_oauth_url(url: &url::Url) -> anyhow::Result<()> {
    let loopback = match url.host() {
        Some(url::Host::Domain("localhost")) => true,
        Some(url::Host::Ipv4(ip)) => ip.is_loopback(),
        Some(url::Host::Ipv6(ip)) => ip.is_loopback(),
        _ => false,
    };
    anyhow::ensure!(
        url.host().is_some() && (url.scheme() == "https" || (url.scheme() == "http" && loopback)),
        "OAuth URLs must use HTTPS (loopback HTTP is allowed for development)"
    );
    anyhow::ensure!(
        url.username().is_empty()
            && url.password().is_none()
            && url.fragment().is_none()
            && url.query().is_none(),
        "OAuth URLs cannot contain credentials, query or fragment"
    );
    Ok(())
}

impl PublicUrl {
    pub fn from_env() -> anyhow::Result<Self> {
        let read = |name| match std::env::var(name) {
            Ok(value) => Ok(Some(value)),
            Err(std::env::VarError::NotPresent) => Ok(None),
            Err(error) => Err(anyhow::Error::from(error)),
        };
        Self::resolve(
            read("SUMI_BASE_URL")?.as_deref(),
            read("SUMI_RESOURCE_URL")?.as_deref(),
        )
    }

    /// BASE_URL excludes /mcp; RESOURCE_URL remains supported for existing installs.
    pub fn resolve(base: Option<&str>, resource: Option<&str>) -> anyhow::Result<Self> {
        let from_base = base
            .map(|base| -> anyhow::Result<String> {
                let mut url = url::Url::parse(base)?;
                validate_oauth_url(&url)?;
                let path = url.path().trim_end_matches('/').to_owned();
                anyhow::ensure!(
                    !path.ends_with("/mcp"),
                    "SUMI_BASE_URL excludes /mcp; use SUMI_RESOURCE_URL for a complete MCP URL"
                );
                url.set_path(&format!("{path}/mcp"));
                Ok(url.to_string())
            })
            .transpose()?;
        if let (Some(derived), Some(explicit)) = (&from_base, resource) {
            anyhow::ensure!(
                derived == explicit,
                "SUMI_BASE_URL and SUMI_RESOURCE_URL disagree"
            );
        }
        let resource_url = resource
            .map(str::to_owned)
            .or(from_base)
            .ok_or_else(|| anyhow::anyhow!("Set SUMI_BASE_URL (or legacy SUMI_RESOURCE_URL)"))?;
        let url = url::Url::parse(&resource_url)?;
        validate_oauth_url(&url)?;
        let base_path = url.path().strip_suffix("/mcp").ok_or_else(|| {
            anyhow::anyhow!("SUMI_RESOURCE_URL must end with /mcp, without a trailing slash")
        })?;
        anyhow::ensure!(
            base_path.is_empty()
                || (base_path.starts_with('/')
                    && base_path[1..].split('/').all(|segment| {
                        !segment.is_empty()
                            && segment.bytes().all(|b| {
                                b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.' | b'~')
                            })
                    })),
            "Public URL path segments must use ASCII letters, digits, -, _, . or ~"
        );
        let origin = url.origin().ascii_serialization();
        Ok(Self {
            base_url: format!("{origin}{base_path}"),
            mcp_path: url.path().to_owned(),
            metadata_path: format!("/.well-known/oauth-protected-resource{}", url.path()),
            health_path: format!("{base_path}/healthz"),
            authority: url[url::Position::BeforeHost..url::Position::AfterPort].to_owned(),
            origin,
            resource_url,
        })
    }
}
