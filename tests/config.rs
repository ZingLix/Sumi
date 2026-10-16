use sumi::config::PublicUrl;

#[test]
fn public_base_url_resolves_domain_port_prefix_and_legacy_resource() {
    for (base, resource, mcp, metadata) in [
        (
            "https://ledger.example.com/",
            "https://ledger.example.com/mcp",
            "/mcp",
            "/.well-known/oauth-protected-resource/mcp",
        ),
        (
            "https://example.com:8443/apps/sumi/",
            "https://example.com:8443/apps/sumi/mcp",
            "/apps/sumi/mcp",
            "/.well-known/oauth-protected-resource/apps/sumi/mcp",
        ),
        (
            "http://[::1]:8080/sumi",
            "http://[::1]:8080/sumi/mcp",
            "/sumi/mcp",
            "/.well-known/oauth-protected-resource/sumi/mcp",
        ),
    ] {
        let url = PublicUrl::resolve(Some(base), None).unwrap();
        assert_eq!(url.resource_url, resource);
        assert_eq!(url.mcp_path, mcp);
        assert_eq!(url.metadata_path, metadata);
        assert_eq!(
            PublicUrl::resolve(None, Some(resource))
                .unwrap()
                .resource_url,
            resource
        );
        assert!(PublicUrl::resolve(Some(base), Some(resource)).is_ok());
    }
    let url = PublicUrl::resolve(Some("https://example.com:8443/sumi"), None).unwrap();
    assert_eq!(url.origin, "https://example.com:8443");
    assert_eq!(url.authority, "example.com:8443");
    assert_eq!(url.base_url, "https://example.com:8443/sumi");
    assert_eq!(url.health_path, "/sumi/healthz");
}

#[test]
fn public_urls_reject_conflicting_or_unsafe_deployment_settings() {
    assert!(PublicUrl::resolve(None, None).is_err());
    assert!(
        PublicUrl::resolve(
            Some("https://example.com/sumi"),
            Some("https://example.com/mcp")
        )
        .is_err()
    );
    for base in [
        "",
        "http://example.com",
        "https://user:password@example.com",
        "https://example.com?x=y",
        "https://example.com/#fragment",
        "https://example.com/mcp",
        "https://example.com/a//b",
        "https://example.com/{route}",
        "https://example.com/a%2Fb",
    ] {
        assert!(
            PublicUrl::resolve(Some(base), None).is_err(),
            "accepted {base}"
        );
    }
    for resource in [
        "https://example.com",
        "https://example.com/mcp/",
        "https://example.com/other",
        "https://example.com/sumi/mcp?x=y",
    ] {
        assert!(
            PublicUrl::resolve(None, Some(resource)).is_err(),
            "accepted {resource}"
        );
    }
}
