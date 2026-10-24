//! Outbound URL and DNS policy for metadata and untrusted download sources.

use std::{
    io,
    net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr},
};

use reqwest::dns::{Addrs, Name, Resolve, Resolving};
use url::{Host, Url};

use crate::Error;

/// Explicit network trust boundary, shared by initial requests and redirects.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum NetworkPolicy {
    /// HTTPS on port 443, with every resolved address publicly routable.
    #[default]
    PublicHttps,
    /// HTTP/HTTPS to loopback addresses only, for explicitly injected test clients.
    LoopbackOnly,
}

impl NetworkPolicy {
    /// Checks scheme, authority, credentials, port, and literal IP restrictions.
    ///
    /// Hostname resolution is separately enforced by the client's filtering DNS
    /// resolver immediately before a connection uses the returned addresses.
    ///
    /// # Errors
    /// Returns an error if the URL violates this policy.
    pub fn validate_url(self, url: &Url) -> Result<(), Error> {
        let reject = |reason| rejected(url, reason);
        if !url.username().is_empty() || url.password().is_some() {
            return Err(reject("embedded credentials are forbidden"));
        }
        if url.fragment().is_some() {
            return Err(reject("URL fragments are forbidden"));
        }
        match self {
            Self::PublicHttps => {
                if url.scheme() != "https" || url.port_or_known_default() != Some(443) {
                    return Err(reject("public downloads require HTTPS on port 443"));
                }
            }
            Self::LoopbackOnly => {
                if !matches!(url.scheme(), "http" | "https") {
                    return Err(reject("loopback tests require HTTP or HTTPS"));
                }
            }
        }
        match url.host() {
            Some(Host::Ipv4(address)) => {
                if !self.allows_ip(IpAddr::V4(address)) {
                    return Err(reject("IP address is outside the allowed network scope"));
                }
            }
            Some(Host::Ipv6(address)) => {
                if !self.allows_ip(IpAddr::V6(address)) {
                    return Err(reject("IP address is outside the allowed network scope"));
                }
            }
            Some(Host::Domain(domain)) => {
                let name = domain.trim_end_matches('.').to_ascii_lowercase();
                let allowed = match self {
                    Self::PublicHttps => is_public_hostname(&name),
                    Self::LoopbackOnly => name == "localhost",
                };
                if !allowed {
                    return Err(reject("hostname is outside the allowed network scope"));
                }
            }
            None => return Err(reject("URL has no hostname")),
        }
        Ok(())
    }

    const fn allows_ip(self, address: IpAddr) -> bool {
        match self {
            Self::PublicHttps => match address {
                IpAddr::V4(address) => is_public_v4(address),
                IpAddr::V6(address) => is_public_v6(address),
            },
            Self::LoopbackOnly => address.is_loopback(),
        }
    }
}

fn rejected(url: &Url, reason: &'static str) -> Error {
    let mut sanitized = url.clone();
    if (!sanitized.username().is_empty() || sanitized.password().is_some())
        && (sanitized.set_password(None).is_err() || sanitized.set_username("").is_err())
    {
        return Error::InvalidSpec(reason.to_owned());
    }
    // Query values can contain signed download credentials; diagnostics need
    // only the origin/path identifying the rejected resource.
    sanitized.set_query(None);
    sanitized.set_fragment(None);
    Error::UrlRejected {
        url: sanitized,
        reason,
    }
}

// Only used after URL policy validation (which rejects user-info). Signed
// download query strings are not needed to identify a failed origin/path.
pub(crate) fn diagnostic_url(url: &Url) -> Url {
    let mut sanitized = url.clone();
    sanitized.set_query(None);
    sanitized.set_fragment(None);
    sanitized
}

fn is_public_hostname(name: &str) -> bool {
    if !name.contains('.') {
        return false;
    }
    !["localhost", "local", "internal", "lan", "home", "home.arpa"]
        .iter()
        .any(|suffix| name == *suffix || name.ends_with(&format!(".{suffix}")))
}

const fn is_public_v4(address: Ipv4Addr) -> bool {
    let [first, second, third, _] = address.octets();
    !(first == 0
        || first == 10
        || (first == 100 && second >= 64 && second <= 127)
        || first == 127
        || (first == 169 && second == 254)
        || (first == 172 && second >= 16 && second <= 31)
        || (first == 192 && second == 0 && (third == 0 || third == 2))
        || (first == 192 && second == 88 && third == 99)
        || (first == 192 && second == 168)
        || (first == 198 && (second == 18 || second == 19))
        || (first == 198 && second == 51 && third == 100)
        || (first == 203 && second == 0 && third == 113)
        || first >= 224)
}

const fn is_public_v6(address: Ipv6Addr) -> bool {
    let segments = address.segments();
    // Require allocated global unicast space. This also excludes IPv4-mapped
    // addresses, translation prefixes, multicast, site-local and scoped addresses.
    if segments[0] & 0xe000 != 0x2000 {
        return false;
    }
    // The IETF protocol-assignment block includes transition mechanisms and
    // non-global special uses; a conservative deny avoids embedded IPv4 bypasses.
    !((segments[0] == 0x2001 && (segments[1] <= 0x01ff || segments[1] == 0x0db8))
        || segments[0] == 0x2002
        // 3000::/4 is inside 2000::/3 but is IANA-reserved/unallocated.
        || segments[0] & 0xf000 == 0x3000
        || (segments[0] == 0x3fff && segments[1] <= 0x0fff))
}

/// Returns only validated addresses to the connector, preventing DNS rebinding
/// between validation and connection. Any mixed-scope answer is rejected whole.
#[derive(Debug)]
pub(crate) struct PolicyResolver {
    policy: NetworkPolicy,
}

impl PolicyResolver {
    pub(crate) const fn new(policy: NetworkPolicy) -> Self {
        Self { policy }
    }
}

impl Resolve for PolicyResolver {
    fn resolve(&self, name: Name) -> Resolving {
        let hostname = name.as_str().to_owned();
        let policy = self.policy;
        Box::pin(async move {
            let addresses: Vec<_> = tokio::net::lookup_host((hostname.as_str(), 0))
                .await?
                .collect();
            let validated = validate_addresses(policy, addresses)?;
            Ok(Box::new(validated.into_iter()) as Addrs)
        })
    }
}

fn validate_addresses(
    policy: NetworkPolicy,
    addresses: Vec<SocketAddr>,
) -> io::Result<Vec<SocketAddr>> {
    if addresses.is_empty()
        || addresses
            .iter()
            .any(|address| !policy.allows_ip(address.ip()))
    {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "DNS answer contains no addresses or addresses outside the allowed network scope",
        ));
    }
    Ok(addresses)
}

#[cfg(test)]
mod tests {
    use super::{NetworkPolicy, PolicyResolver, validate_addresses};
    use reqwest::dns::Resolve as _;
    use url::Url;

    #[test]
    fn public_policy_rejects_local_reserved_and_ambiguous_authorities() {
        for url in [
            "http://example.com/file",
            "https://example.com:8443/file",
            "https://user:secret@example.com/file",
            "https://example.com/file#fragment",
            "https://localhost/file",
            "https://foo.localhost./file",
            "https://service.internal/file",
            "https://router.home.arpa/file",
            "https://single-label/file",
            "https://127.0.0.1/file",
            "https://2130706433/file",
            "https://0x7f000001/file",
            "https://10.0.0.1/file",
            "https://100.64.0.1/file",
            "https://169.254.169.254/file",
            "https://172.16.0.1/file",
            "https://192.168.0.1/file",
            "https://192.0.2.1/file",
            "https://198.18.0.1/file",
            "https://198.51.100.1/file",
            "https://203.0.113.1/file",
            "https://224.0.0.1/file",
            "https://240.0.0.1/file",
            "https://255.255.255.255/file",
            "https://[::]/file",
            "https://[::1]/file",
            "https://[::ffff:127.0.0.1]/file",
            "https://[::ffff:8.8.8.8]/file",
            "https://[64:ff9b::7f00:1]/file",
            "https://[fc00::1]/file",
            "https://[fe80::1]/file",
            "https://[fec0::1]/file",
            "https://[ff00::1]/file",
            "https://[2001:db8::1]/file",
            "https://[2002:7f00:1::]/file",
            "https://[3000::1]/file",
            "https://[3fff::1]/file",
        ] {
            assert!(
                NetworkPolicy::PublicHttps
                    .validate_url(&Url::parse(url).expect("test URL"))
                    .is_err(),
                "accepted {url}"
            );
        }
    }

    #[test]
    fn public_policy_accepts_public_https_services() {
        for url in [
            "https://piston-meta.mojang.com/mc/game/version_manifest_v2.json",
            "https://libraries.minecraft.net/a.jar",
            "https://cdn.modrinth.com/data/a.jar",
            "https://github.com/owner/repo/releases/download/v1/a.jar",
            "https://8.8.8.8/file",
            "https://[2001:4860:4860::8888]/file",
        ] {
            NetworkPolicy::PublicHttps
                .validate_url(&Url::parse(url).expect("test URL"))
                .expect("public HTTPS URL");
        }
    }

    #[test]
    fn rejected_url_errors_do_not_reveal_credentials_or_query_values() {
        let url =
            Url::parse("https://user:password@example.com/file?token=secret").expect("test URL");
        let error = NetworkPolicy::PublicHttps
            .validate_url(&url)
            .expect_err("embedded credentials");
        let diagnostic = error.to_string();
        assert!(!diagnostic.contains("password"));
        assert!(!diagnostic.contains("secret"));
        assert!(!diagnostic.contains("user@"));
    }

    #[test]
    fn loopback_policy_is_explicit_and_cannot_contact_public_addresses() {
        for url in [
            "http://127.0.0.1:1234/a",
            "http://localhost:2345/a",
            "https://[::1]:3456/a",
        ] {
            NetworkPolicy::LoopbackOnly
                .validate_url(&Url::parse(url).expect("test URL"))
                .expect("loopback test URL");
        }
        for url in ["https://example.com/a", "http://10.0.0.1/a"] {
            assert!(
                NetworkPolicy::LoopbackOnly
                    .validate_url(&Url::parse(url).expect("test URL"))
                    .is_err()
            );
        }
    }

    #[test]
    fn mixed_public_private_dns_results_are_rejected_as_a_whole() {
        let public = "8.8.8.8:0".parse().expect("public address");
        let private = "10.0.0.1:0".parse().expect("private address");
        assert!(validate_addresses(NetworkPolicy::PublicHttps, vec![public, private]).is_err());
        assert!(validate_addresses(NetworkPolicy::PublicHttps, vec![]).is_err());
        assert_eq!(
            validate_addresses(NetworkPolicy::PublicHttps, vec![public]).expect("public DNS"),
            [public]
        );
    }

    #[tokio::test]
    async fn resolver_rejects_localhost_even_when_requested_without_url_validation() {
        let resolver = PolicyResolver::new(NetworkPolicy::PublicHttps);
        assert!(
            resolver
                .resolve("localhost".parse().expect("DNS name"))
                .await
                .is_err()
        );
    }
}
