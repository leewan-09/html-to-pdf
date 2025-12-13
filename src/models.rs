use serde::{Deserialize, Serialize, de};
use url::Url;
use std::net::{Ipv4Addr, Ipv6Addr, ToSocketAddrs, IpAddr};

#[derive(Deserialize)]
pub struct PdfRequest {
    pub name: String,
    #[serde(deserialize_with = "validate_url")]
    pub url: String,
}

impl PdfRequest {
    pub fn validate(&self) -> Result<(), String> {
        if self.name.is_empty() {
            return Err("Name cannot be empty".to_string());
        }

        // Limit name length to prevent abuse
        if self.name.len() > 256 {
            return Err("Name too long (max 256 characters)".to_string());
        }

        // DNS resolution check to prevent DNS rebinding attacks
        // This resolves the domain and validates all resulting IPs
        self.validate_resolved_ips()?;

        Ok(())
    }

    /// Resolve domain to IPs and validate against private/internal ranges
    fn validate_resolved_ips(&self) -> Result<(), String> {
        let url = Url::parse(&self.url).map_err(|_| "Invalid URL")?;

        if let Some(host) = url.host_str() {
            // Skip IP addresses - they're already validated during deserialization
            if host.parse::<Ipv4Addr>().is_ok() || host.parse::<Ipv6Addr>().is_ok() {
                return Ok(());
            }

            // Resolve domain to IP addresses
            let port = url.port().unwrap_or(if url.scheme() == "https" { 443 } else { 80 });
            let addr_str = format!("{}:{}", host, port);

            match addr_str.to_socket_addrs() {
                Ok(addrs) => {
                    for socket_addr in addrs {
                        match socket_addr.ip() {
                            IpAddr::V4(ip) => {
                                if is_private_ipv4(ip) {
                                    return Err(format!(
                                        "Domain '{}' resolves to private IP address",
                                        host
                                    ));
                                }
                            }
                            IpAddr::V6(ip) => {
                                if is_private_ipv6(ip) {
                                    return Err(format!(
                                        "Domain '{}' resolves to private IP address",
                                        host
                                    ));
                                }
                            }
                        }
                    }
                }
                Err(_) => {
                    // DNS resolution failed - allow Chrome to handle it
                    // This could be a temporary DNS issue
                }
            }
        }

        Ok(())
    }
}

fn validate_url<'de, D>(deserializer: D) -> Result<String, D::Error>
where
    D: de::Deserializer<'de>,
{
    let url_str = String::deserialize(deserializer)?;
    
    // Parse URL to validate format
    let url = Url::parse(&url_str)
        .map_err(|_| de::Error::custom("Invalid URL format"))?;
    
    // Validate scheme - only allow http and https
    match url.scheme() {
        "http" | "https" => {},
        scheme => return Err(de::Error::custom(format!(
            "Invalid URL scheme '{}'. Only 'http' and 'https' are allowed",
            scheme
        ))),
    }
    
    // SSRF Protection: Check for private/internal networks
    if let Some(host) = url.host() {
        match host {
            url::Host::Ipv4(ip) => {
                if is_private_ipv4(ip) {
                    return Err(de::Error::custom("Access to private networks is not allowed"));
                }
            },
            url::Host::Ipv6(ip) => {
                if is_private_ipv6(ip) {
                    return Err(de::Error::custom("Access to private networks is not allowed"));
                }
            },
            url::Host::Domain(domain) => {
                let domain_lower = domain.to_lowercase();

                // Block localhost domains
                if is_localhost_domain(&domain_lower) {
                    return Err(de::Error::custom("Access to localhost is not allowed"));
                }

                // Block cloud metadata service domains
                if is_cloud_metadata_domain(&domain_lower) {
                    return Err(de::Error::custom("Access to cloud metadata services is not allowed"));
                }
            }
        }
    } else {
        return Err(de::Error::custom("URL must have a valid host"));
    }
    
    Ok(url_str)
}

fn is_private_ipv4(ip: Ipv4Addr) -> bool {
    // Check for private/internal IPv4 ranges
    // Note: is_link_local() covers 169.254.0.0/16, is_multicast() covers 224.0.0.0/4
    ip.is_loopback() ||
    ip.is_private() ||
    ip.is_link_local() ||
    ip.is_multicast() ||
    ip.is_broadcast() ||
    ip.is_unspecified() || // 0.0.0.0
    // Reserved range 240.0.0.0/4 (not covered by std methods)
    ip.octets()[0] >= 240
}

fn is_private_ipv6(ip: Ipv6Addr) -> bool {
    // Check for private/internal IPv6 ranges
    let segments = ip.segments();

    ip.is_loopback() ||
    ip.is_multicast() ||
    ip.is_unspecified() || // ::
    // Link-local addresses (fe80::/10) - first 10 bits are 1111111010
    (segments[0] & 0xffc0) == 0xfe80 ||
    // Unique local addresses (fc00::/7) - first 7 bits are 1111110
    (segments[0] & 0xfe00) == 0xfc00
}

fn is_localhost_domain(domain: &str) -> bool {
    matches!(domain, "localhost" | "127.0.0.1" | "::1" | "0.0.0.0" | "::") || domain.ends_with(".localhost")
}

/// Block cloud provider metadata service domains (AWS, GCP, Azure, etc.)
fn is_cloud_metadata_domain(domain: &str) -> bool {
    matches!(domain,
        // AWS/Azure/DigitalOcean metadata IP
        "169.254.169.254" |
        // GCP metadata
        "metadata.google.internal" |
        "metadata" |
        // Azure metadata
        "metadata.azure.com" |
        // Alibaba Cloud
        "100.100.100.200" |
        // Oracle Cloud
        "192.0.0.192"
    ) ||
    // Block any .internal domain (GCP uses these)
    domain.ends_with(".internal") ||
    // Block metadata subdomains
    domain.starts_with("metadata.")
}

#[derive(Serialize)]
pub struct ErrorResponse {
    pub error: String,
}