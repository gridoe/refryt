//! Which network this computer is on, for the settings that only hold on networks the user
//! allowed (the cast receiver).
//!
//! A network is told apart by its router: the hardware address of the default gateway. It is
//! there on a cable as on Wi-Fi, every system hands it out without a permission prompt, and a
//! hotspot named like the home Wi-Fi does not share it. Only a hash of it is kept, so the
//! settings file does not carry the router's address. The name shown beside it is the
//! connection's own where the system gives it freely (NetworkManager's on Linux), and the
//! router's IP otherwise.

use std::net::Ipv4Addr;

/// Whether this system can say which network it is on. Where it cannot, a per-network setting
/// falls back to being plain on or off.
pub const SUPPORTED: bool = cfg!(any(
    target_os = "linux",
    target_os = "windows",
    target_os = "macos"
));

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Network {
    /// A hash of the router's hardware address.
    pub id: String,
    /// What to call it in Settings.
    pub name: String,
}

/// The network the default route goes out on, or `None` with no network (or no way to tell).
/// Blocking, and may start a short-lived process: run it off the UI thread.
pub fn current() -> Option<Network> {
    let (gateway, mac, name) = gateway()?;
    if mac.iter().all(|&b| b == 0) {
        return None;
    }
    Some(Network {
        id: id_of(&mac),
        name: name.unwrap_or_else(|| format!("Network at {gateway}")),
    })
}

fn id_of(mac: &[u8; 6]) -> String {
    use sha2::{Digest, Sha256};
    let digest = Sha256::digest([b"refryt-network:".as_slice(), mac].concat());
    digest[..8].iter().map(|b| format!("{b:02x}")).collect()
}

/// The default gateway, its hardware address, and the connection's name if known.
#[cfg(target_os = "linux")]
fn gateway() -> Option<(Ipv4Addr, [u8; 6], Option<String>)> {
    let route = std::fs::read_to_string("/proc/net/route").ok()?;
    let (iface, gateway) = default_route(&route)?;
    let arp = std::fs::read_to_string("/proc/net/arp").ok()?;
    let mac = arp_entry(&arp, gateway, &iface)?;
    Some((gateway, mac, connection_name(&iface)))
}

/// The interface and gateway of the default route with the lowest metric, from
/// `/proc/net/route` (addresses in little-endian hex).
#[cfg(any(target_os = "linux", test))]
fn default_route(table: &str) -> Option<(String, Ipv4Addr)> {
    table
        .lines()
        .skip(1)
        .filter_map(|line| {
            let f: Vec<&str> = line.split_whitespace().collect();
            if f.len() < 8 || f[1] != "00000000" {
                return None;
            }
            let gateway = u32::from_str_radix(f[2], 16).ok()?;
            let flags = u16::from_str_radix(f[3], 16).ok()?;
            // RTF_UP and RTF_GATEWAY.
            if flags & 0x3 != 0x3 || gateway == 0 {
                return None;
            }
            let metric: u32 = f[6].parse().ok()?;
            Some((
                metric,
                f[0].to_owned(),
                Ipv4Addr::from(gateway.swap_bytes()),
            ))
        })
        .min_by_key(|(metric, ..)| *metric)
        .map(|(_, iface, gateway)| (iface, gateway))
}

/// The hardware address `/proc/net/arp` has for `ip` on `iface`.
#[cfg(any(target_os = "linux", test))]
fn arp_entry(table: &str, ip: Ipv4Addr, iface: &str) -> Option<[u8; 6]> {
    let ip = ip.to_string();
    table.lines().skip(1).find_map(|line| {
        let f: Vec<&str> = line.split_whitespace().collect();
        (f.len() >= 6 && f[0] == ip && f[5] == iface)
            .then(|| parse_mac(f[3]))
            .flatten()
    })
}

/// NetworkManager's name for the connection on `iface` (the Wi-Fi's name, or "Wired connection
/// 1"), where NetworkManager runs.
#[cfg(target_os = "linux")]
fn connection_name(iface: &str) -> Option<String> {
    let out = std::process::Command::new("nmcli")
        .args(["-t", "-f", "NAME,DEVICE", "connection", "show", "--active"])
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    String::from_utf8_lossy(&out.stdout)
        .lines()
        .find_map(|line| {
            // `-t` escapes a colon inside a field as `\:`, so the device is after the last colon.
            let (name, device) = line.rsplit_once(':')?;
            (device == iface).then(|| name.replace("\\:", ":"))
        })
        .filter(|name| !name.is_empty())
}

#[cfg(target_os = "macos")]
fn gateway() -> Option<(Ipv4Addr, [u8; 6], Option<String>)> {
    let run = |cmd: &str, args: &[&str]| -> Option<String> {
        let out = std::process::Command::new(cmd).args(args).output().ok()?;
        out.status
            .success()
            .then(|| String::from_utf8_lossy(&out.stdout).into_owned())
    };
    let route = run("/sbin/route", &["-n", "get", "default"])?;
    let gateway: Ipv4Addr = route
        .lines()
        .find_map(|l| l.trim().strip_prefix("gateway:"))?
        .trim()
        .parse()
        .ok()?;
    let arp = run("/usr/sbin/arp", &["-n", &gateway.to_string()])?;
    // "? (192.168.1.1) at 80:af:ca:71:35:16 on en0 ifscope [ethernet]"
    let mac = arp
        .split_whitespace()
        .skip_while(|w| *w != "at")
        .nth(1)
        .and_then(parse_mac)?;
    Some((gateway, mac, None))
}

#[cfg(target_os = "windows")]
fn gateway() -> Option<(Ipv4Addr, [u8; 6], Option<String>)> {
    use windows_sys::Win32::NetworkManagement::IpHelper::{
        GetBestRoute, MIB_IPFORWARDROW, SendARP,
    };
    // The route a packet to the internet would take, without sending one.
    let probe = u32::from_ne_bytes([1, 1, 1, 1]);
    // SAFETY: `row` is a plain struct GetBestRoute fills in.
    let mut row: MIB_IPFORWARDROW = unsafe { std::mem::zeroed() };
    if unsafe { GetBestRoute(probe, 0, &mut row) } != 0 {
        return None;
    }
    let next_hop = row.dwForwardNextHop;
    if next_hop == 0 {
        return None;
    }
    let gateway = Ipv4Addr::from(next_hop.to_ne_bytes());
    let mut mac = [0u8; 8];
    let mut len = mac.len() as u32;
    // SAFETY: `mac` is `len` bytes long; SendARP writes at most that many.
    if unsafe { SendARP(next_hop, 0, mac.as_mut_ptr().cast(), &mut len) } != 0 || len < 6 {
        return None;
    }
    let mut out = [0u8; 6];
    out.copy_from_slice(&mac[..6]);
    Some((gateway, out, None))
}

#[cfg(not(any(target_os = "linux", target_os = "windows", target_os = "macos")))]
fn gateway() -> Option<(Ipv4Addr, [u8; 6], Option<String>)> {
    None
}

/// `80:af:ca:71:35:16`, or macOS's unpadded `0:af:ca:1:35:16`.
#[cfg_attr(target_os = "windows", allow(dead_code))]
fn parse_mac(text: &str) -> Option<[u8; 6]> {
    let mut out = [0u8; 6];
    let mut parts = text.split([':', '-']);
    for byte in &mut out {
        *byte = u8::from_str_radix(parts.next()?, 16).ok()?;
    }
    parts.next().is_none().then_some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    const ROUTE: &str = "Iface\tDestination\tGateway \tFlags\tRefCnt\tUse\tMetric\tMask\t\tMTU\tWindow\tIRTT\n\
        wlan0\t00000000\t0102A8C0\t0003\t0\t0\t600\t00000000\t0\t0\t0\n\
        enp4s0\t00000000\t010AA8C0\t0003\t0\t0\t100\t00000000\t0\t0\t0\n\
        enp4s0\t000AA8C0\t00000000\t0001\t0\t0\t100\t00FFFFFF\t0\t0\t0\n";

    const ARP: &str = "IP address       HW type     Flags       HW address            Mask     Device\n\
        192.168.10.1     0x1         0x2         80:af:ca:71:35:16     *        enp4s0\n\
        192.168.2.1      0x1         0x2         11:22:33:44:55:66     *        wlan0\n";

    #[test]
    fn the_default_route_with_the_lowest_metric_wins() {
        assert_eq!(
            default_route(ROUTE),
            Some(("enp4s0".to_owned(), Ipv4Addr::new(192, 168, 10, 1)))
        );
    }

    #[test]
    fn the_gateways_hardware_address_is_found_on_its_interface() {
        let gateway = Ipv4Addr::new(192, 168, 10, 1);
        assert_eq!(
            arp_entry(ARP, gateway, "enp4s0"),
            Some([0x80, 0xaf, 0xca, 0x71, 0x35, 0x16])
        );
        assert_eq!(arp_entry(ARP, gateway, "wlan0"), None);
    }

    #[test]
    fn hardware_addresses_read_padded_or_not() {
        assert_eq!(
            parse_mac("0:af:ca:1:35:16"),
            Some([0, 0xaf, 0xca, 1, 0x35, 0x16])
        );
        assert_eq!(
            parse_mac("80-AF-CA-71-35-16"),
            Some([0x80, 0xaf, 0xca, 0x71, 0x35, 0x16])
        );
        assert_eq!(parse_mac("80:af:ca"), None);
        assert_eq!(parse_mac("80:af:ca:71:35:16:00"), None);
    }

    #[test]
    fn a_network_id_is_stable_and_not_the_address() {
        let mac = [0x80, 0xaf, 0xca, 0x71, 0x35, 0x16];
        assert_eq!(id_of(&mac), id_of(&mac));
        assert_ne!(id_of(&mac), id_of(&[0x80, 0xaf, 0xca, 0x71, 0x35, 0x17]));
        assert!(!id_of(&mac).contains("80af"));
    }

    #[test]
    fn this_machine_knows_its_network() {
        // Runs the real lookup wherever there is one, so each system's CI job goes through its
        // own route and ARP path. Only checks it does not panic and prints what it found: a
        // build machine may well be offline, or behind a gateway it cannot see.
        let here = current();
        println!("this network: {here:?}");
        if let Some(here) = here {
            assert_eq!(here.id.len(), 16);
            assert!(!here.name.is_empty());
        }
    }
}
