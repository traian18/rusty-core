//! SSRF guard: rejects outbound targets in loopback, link-local (including
//! the `169.254.0.0/16` cloud instance-metadata range), and private address
//! ranges.
//!
//! `web.fetch` takes an arbitrary URL from agent-controlled input —
//! ultimately model output, so attacker-influenced if a prompt injection
//! occurs — and makes a server-side HTTP request from it. Without this
//! guard, a malicious prompt could direct a fetch at
//! `http://169.254.169.254/latest/meta-data/` (cloud instance metadata),
//! `http://localhost:<port>/...` (an unauthenticated internal service on the
//! same host), or an internal RFC1918 address reachable from the harness
//! host but not from outside. This is deliberately a hard default-deny
//! check, not an afterthought.

use std::net::{IpAddr, Ipv6Addr};

/// Returns `true` if `ip` is safe to connect to for an outbound fetch.
pub(crate) fn is_safe_target(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => {
            !v4.is_loopback()
                && !v4.is_link_local() // covers 169.254.0.0/16, the cloud metadata range
                && !v4.is_private()
                && !v4.is_broadcast()
                && !v4.is_documentation()
                && !v4.is_unspecified()
        }
        IpAddr::V6(v6) => {
            !v6.is_loopback()
                && !v6.is_unspecified()
                && !is_unique_local(v6)
                && !is_unicast_link_local(v6)
        }
    }
}

/// `fc00::/7` — IPv6 equivalent of RFC1918 private ranges.
///
/// Implemented via a manual bitmask rather than `Ipv6Addr::is_unique_local`
/// because that method is not yet stable on all supported toolchains.
fn is_unique_local(ip: Ipv6Addr) -> bool {
    (ip.segments()[0] & 0xfe00) == 0xfc00
}

/// `fe80::/10` — IPv6 link-local (the IPv6 analogue of `169.254.0.0/16`).
fn is_unicast_link_local(ip: Ipv6Addr) -> bool {
    (ip.segments()[0] & 0xffc0) == 0xfe80
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::Ipv4Addr;

    #[test]
    fn rejects_loopback() {
        assert!(!is_safe_target(IpAddr::V4(Ipv4Addr::new(127, 0, 0, 1))));
        assert!(!is_safe_target(IpAddr::V6(Ipv6Addr::LOCALHOST)));
    }

    #[test]
    fn rejects_cloud_metadata_link_local_range() {
        assert!(!is_safe_target(IpAddr::V4(Ipv4Addr::new(169, 254, 169, 254))));
    }

    #[test]
    fn rejects_rfc1918_private_ranges() {
        assert!(!is_safe_target(IpAddr::V4(Ipv4Addr::new(10, 0, 0, 5))));
        assert!(!is_safe_target(IpAddr::V4(Ipv4Addr::new(172, 16, 0, 5))));
        assert!(!is_safe_target(IpAddr::V4(Ipv4Addr::new(192, 168, 1, 5))));
    }

    #[test]
    fn rejects_ipv6_unique_local_and_link_local() {
        assert!(!is_safe_target(IpAddr::V6(Ipv6Addr::new(
            0xfc00, 0, 0, 0, 0, 0, 0, 1
        ))));
        assert!(!is_safe_target(IpAddr::V6(Ipv6Addr::new(
            0xfe80, 0, 0, 0, 0, 0, 0, 1
        ))));
    }

    #[test]
    fn allows_a_normal_public_ipv4_address() {
        // 8.8.8.8 — a public address, not in any disallowed range.
        assert!(is_safe_target(IpAddr::V4(Ipv4Addr::new(8, 8, 8, 8))));
    }

    #[test]
    fn allows_a_normal_public_ipv6_address() {
        // 2001:4860:4860::8888 — Google public DNS, IPv6.
        assert!(is_safe_target(IpAddr::V6(Ipv6Addr::new(
            0x2001, 0x4860, 0x4860, 0, 0, 0, 0, 0x8888
        ))));
    }
}
