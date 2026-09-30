//! Which addresses a fetch may reach. Anything that is not globally routable
//! unicast is refused, so a link cannot reach this machine, the local network
//! or cloud metadata services.
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};

pub fn is_public(address: IpAddr) -> bool {
    match address {
        IpAddr::V4(address) => public_v4(address),
        IpAddr::V6(address) => public_v6(address),
    }
}

fn public_v4(address: Ipv4Addr) -> bool {
    let [a, b, c, _] = address.octets();
    !(address.is_unspecified()
        || address.is_loopback()
        || address.is_private()
        || address.is_link_local()
        || address.is_broadcast()
        || address.is_multicast()
        || address.is_documentation()
        || a == 0
        // Shared address space (carrier-grade NAT), 100.64.0.0/10.
        || (a == 100 && (64..128).contains(&b))
        // IETF protocol assignments, 192.0.0.0/24.
        || (a == 192 && b == 0 && c == 0)
        // Benchmarking, 198.18.0.0/15.
        || (a == 198 && (b == 18 || b == 19))
        // Reserved for future use, 240.0.0.0/4.
        || a >= 240)
}

fn public_v6(address: Ipv6Addr) -> bool {
    if let Some(embedded) = embedded_v4(address) {
        return public_v4(embedded);
    }
    let segments = address.segments();
    !(address.is_unspecified()
        || address.is_loopback()
        || address.is_multicast()
        // Unique local, fc00::/7.
        || (segments[0] & 0xfe00) == 0xfc00
        // Link local, fe80::/10, and deprecated site local, fec0::/10.
        || (segments[0] & 0xffc0) == 0xfe80
        || (segments[0] & 0xffc0) == 0xfec0
        // Documentation, 2001:db8::/32 and 3fff::/20.
        || (segments[0] == 0x2001 && segments[1] == 0x0db8)
        || (segments[0] & 0xfff0) == 0x3ff0
        // Teredo 2001::/32 and benchmarking 2001:2::/48 carry other networks.
        || (segments[0] == 0x2001 && segments[1] == 0)
        || (segments[0] == 0x2001 && segments[1] == 2 && segments[2] == 0)
        // Discard-only, 100::/64.
        || (segments[0] == 0x0100 && segments[1..4] == [0, 0, 0])
        // Anything outside global unicast 2000::/3.
        || (segments[0] & 0xe000) != 0x2000)
}

/// IPv4 addresses carried inside IPv6: mapped, compatible, NAT64 and 6to4.
fn embedded_v4(address: Ipv6Addr) -> Option<Ipv4Addr> {
    let segments = address.segments();
    let v4 = |high: u16, low: u16| {
        Ipv4Addr::new((high >> 8) as u8, high as u8, (low >> 8) as u8, low as u8)
    };
    if let Some(mapped) = address.to_ipv4_mapped() {
        return Some(mapped);
    }
    // IPv4-compatible ::a.b.c.d (deprecated), excluding :: and ::1.
    if segments[..6] == [0; 6] && !address.is_unspecified() && !address.is_loopback() {
        return Some(v4(segments[6], segments[7]));
    }
    // NAT64 well-known prefix 64:ff9b::/96.
    if segments[..6] == [0x64, 0xff9b, 0, 0, 0, 0] {
        return Some(v4(segments[6], segments[7]));
    }
    // 6to4, 2002::/16.
    if segments[0] == 0x2002 {
        return Some(v4(segments[1], segments[2]));
    }
    None
}

#[cfg(test)]
mod tests {
    use super::is_public;

    #[test]
    fn only_global_unicast_addresses_are_public() {
        for public in [
            "93.184.215.14",
            "1.1.1.1",
            "8.8.8.8",
            "2606:4700:4700::1111",
            "2a00:1450:4001:80b::200e",
            "::ffff:93.184.215.14",
            "64:ff9b::808:808",
            "2002:5db8:d70e::1",
        ] {
            assert!(is_public(public.parse().unwrap()), "{public}");
        }
        for private in [
            "0.0.0.0",
            "0.1.2.3",
            "10.0.0.1",
            "100.64.0.1",
            "100.127.255.254",
            "127.0.0.1",
            "127.255.255.254",
            "169.254.169.254",
            "172.16.0.1",
            "172.31.255.255",
            "192.0.0.8",
            "192.0.2.1",
            "192.168.0.1",
            "198.18.0.1",
            "198.51.100.1",
            "203.0.113.1",
            "224.0.0.1",
            "240.0.0.1",
            "255.255.255.255",
            "::",
            "::1",
            "::ffff:127.0.0.1",
            "::ffff:10.0.0.1",
            "::ffff:169.254.169.254",
            "::127.0.0.1",
            "64:ff9b::a00:1",
            "2002:7f00:1::1",
            "2002:c0a8:1::1",
            "fc00::1",
            "fd12:3456::1",
            "fe80::1",
            "fec0::1",
            "ff02::1",
            "2001:db8::1",
            "2001::1",
            "2001:2::1",
            "100::1",
            "3fff::1",
            "4000::1",
        ] {
            assert!(!is_public(private.parse().unwrap()), "{private}");
        }
    }
}
