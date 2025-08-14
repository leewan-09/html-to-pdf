use serde::{Deserialize, Serialize, de};
use url::Url;
use std::net::{Ipv4Addr, Ipv6Addr};
use ipnet::{Ipv4Net, Ipv6Net};

#[derive(Deserialize)]
pub struct PdfRequest {
    pub name: String,
    #[serde(deserialize_with = "validate_url")]
    pub url: String,
}

impl PdfRequest {
    pub fn validate(&self) -> Result<(), String> {
        // Additional validation if needed
        if self.name.is_empty() {
            return Err("Name cannot be empty".to_string());
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
                // Block common localhost domains
                let domain_lower = domain.to_lowercase();
                if is_localhost_domain(&domain_lower) {
                    return Err(de::Error::custom("Access to localhost is not allowed"));
                }
                
                // Additional domain validation could be added here
                // For example, resolving the domain and checking the resulting IPs
            }
        }
    } else {
        return Err(de::Error::custom("URL must have a valid host"));
    }
    
    Ok(url_str)
}

fn is_private_ipv4(ip: Ipv4Addr) -> bool {
    // Check for private/internal IPv4 ranges
    ip.is_loopback() ||
    ip.is_private() ||
    ip.is_link_local() ||
    ip.is_multicast() ||
    ip.is_broadcast() ||
    ip == Ipv4Addr::new(0, 0, 0, 0) || // 0.0.0.0
    // Additional checks for other reserved ranges
    Ipv4Net::new(Ipv4Addr::new(169, 254, 0, 0), 16).unwrap().contains(&ip) || // Link-local
    Ipv4Net::new(Ipv4Addr::new(224, 0, 0, 0), 4).unwrap().contains(&ip) ||   // Multicast
    Ipv4Net::new(Ipv4Addr::new(240, 0, 0, 0), 4).unwrap().contains(&ip)     // Reserved
}

fn is_private_ipv6(ip: Ipv6Addr) -> bool {
    // Check for private/internal IPv6 ranges
    ip.is_loopback() ||
    ip.is_multicast() ||
    ip == Ipv6Addr::new(0, 0, 0, 0, 0, 0, 0, 0) || // ::
    // Link-local addresses (fe80::/10)
    Ipv6Net::new(Ipv6Addr::new(0xfe80, 0, 0, 0, 0, 0, 0, 0), 10).unwrap().contains(&ip) ||
    // Unique local addresses (fc00::/7)
    Ipv6Net::new(Ipv6Addr::new(0xfc00, 0, 0, 0, 0, 0, 0, 0), 7).unwrap().contains(&ip)
}

fn is_localhost_domain(domain: &str) -> bool {
    matches!(domain, "localhost" | "127.0.0.1" | "::1" | "0.0.0.0" | "::") || domain.ends_with(".localhost")
}

#[derive(Serialize)]
pub struct ErrorResponse {
    pub error: String,
}