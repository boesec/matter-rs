//! Network setup: the interface the node announces on, the dual-stack Matter socket and the
//! mDNS socket with its multicast memberships.
//!
//! The Matter socket is bound explicitly: an IPv6 socket with `IPV6_V6ONLY` off when both
//! families are enabled, so that IPv4 datagrams arrive on Windows too (where the default is
//! on), an IPv4 socket for IPv4 only, an IPv6-only socket for IPv6 only. The interface named in
//! the configuration (or the one chosen by the heuristic below) decides which addresses the mDNS
//! responder announces and which multicast groups it joins; an interface without IPv6 is
//! tolerated and simply announces IPv4 only.

use std::io;
use std::net::{Ipv4Addr, Ipv6Addr, SocketAddr, SocketAddrV4, SocketAddrV6, UdpSocket};

use matter_rs_engine::{Error, NetworkConfig};
use rs_matter::transport::network::mdns::{
    MDNS_IPV4_BROADCAST_ADDR, MDNS_IPV6_BROADCAST_ADDR, MDNS_PORT,
};
use socket2::{Domain, Protocol, Socket, Type};

/// The addresses of the interface the node uses.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct InterfaceAddresses {
    /// The interface name.
    pub(crate) name: String,
    /// The interface index, for IPv6 multicast.
    pub(crate) index: u32,
    /// The IPv4 address, if the interface has one and IPv4 is enabled.
    pub(crate) ipv4: Option<Ipv4Addr>,
    /// The IPv6 addresses (link-local first), empty when the interface has none or IPv6 is
    /// disabled.
    pub(crate) ipv6: Vec<Ipv6Addr>,
}

/// One interface as the system reports it, before the configuration is applied.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct SystemInterface {
    pub(crate) name: String,
    pub(crate) index: u32,
    pub(crate) loopback: bool,
    pub(crate) ipv4: Vec<Ipv4Addr>,
    pub(crate) ipv6: Vec<Ipv6Addr>,
}

/// The interfaces of this host, grouped by name.
fn system_interfaces() -> io::Result<Vec<SystemInterface>> {
    let mut interfaces: Vec<SystemInterface> = Vec::new();
    for entry in if_addrs::get_if_addrs()? {
        let interface = match interfaces.iter_mut().find(|i| i.name == entry.name) {
            Some(interface) => interface,
            None => {
                interfaces.push(SystemInterface {
                    name: entry.name.clone(),
                    index: entry.index.unwrap_or(0),
                    loopback: entry.is_loopback(),
                    ..SystemInterface::default()
                });
                interfaces.last_mut().expect("just pushed")
            }
        };
        if let Some(index) = entry.index {
            interface.index = index;
        }
        interface.loopback |= entry.is_loopback();
        match entry.addr {
            if_addrs::IfAddr::V4(v4) => interface.ipv4.push(v4.ip),
            if_addrs::IfAddr::V6(v6) => interface.ipv6.push(v6.ip),
        }
    }
    Ok(interfaces)
}

/// Picks the interface for `config` from `interfaces`: the named one, or the first
/// non-loopback interface with an IPv4 address and a link-local IPv6 address, then the first
/// with an IPv4 address, then the first with any address.
pub(crate) fn choose_interface(
    config: &NetworkConfig,
    interfaces: &[SystemInterface],
) -> Result<InterfaceAddresses, Error> {
    let chosen = match &config.interface {
        Some(name) => {
            interfaces
                .iter()
                .find(|i| &i.name == name)
                .ok_or_else(|| Error::InvalidIdentity {
                    reason: format!("network interface {name:?} does not exist"),
                })?
        }
        None => {
            let candidates = || interfaces.iter().filter(|i| !i.loopback);
            candidates()
                .find(|i| !i.ipv4.is_empty() && i.ipv6.iter().any(|ip| is_unicast_link_local(*ip)))
                .or_else(|| candidates().find(|i| !i.ipv4.is_empty()))
                .or_else(|| candidates().find(|i| !i.ipv6.is_empty()))
                .ok_or_else(|| Error::InvalidIdentity {
                    reason: String::from("no network interface with an address found"),
                })?
        }
    };
    let ipv4 = if config.ipv4 {
        chosen.ipv4.first().copied()
    } else {
        None
    };
    let mut ipv6: Vec<Ipv6Addr> = Vec::new();
    if config.ipv6 {
        ipv6.extend(
            chosen
                .ipv6
                .iter()
                .copied()
                .filter(|ip| !ip.is_loopback() && is_unicast_link_local(*ip)),
        );
        ipv6.extend(
            chosen
                .ipv6
                .iter()
                .copied()
                .filter(|ip| !ip.is_loopback() && !is_unicast_link_local(*ip)),
        );
    }
    if ipv4.is_none() && ipv6.is_empty() {
        return Err(Error::InvalidIdentity {
            reason: format!(
                "network interface {:?} has no address of an enabled IP family",
                chosen.name
            ),
        });
    }
    Ok(InterfaceAddresses {
        name: chosen.name.clone(),
        index: chosen.index,
        ipv4,
        ipv6,
    })
}

/// Picks the interface for `config` among the interfaces of this host.
pub(crate) fn select_interface(config: &NetworkConfig) -> Result<InterfaceAddresses, Error> {
    let interfaces = system_interfaces().map_err(|error| Error::InvalidIdentity {
        reason: format!("the network interfaces cannot be listed: {error}"),
    })?;
    choose_interface(config, &interfaces)
}

fn is_unicast_link_local(ip: Ipv6Addr) -> bool {
    ip.segments()[0] & 0xFFC0 == 0xFE80
}

/// Binds the Matter UDP socket on the configured port for the enabled IP families.
pub(crate) fn bind_matter_socket(config: &NetworkConfig) -> io::Result<UdpSocket> {
    let socket = match (config.ipv4, config.ipv6) {
        (true, false) => {
            let socket = Socket::new(Domain::IPV4, Type::DGRAM, Some(Protocol::UDP))?;
            socket.bind(
                &SocketAddr::V4(SocketAddrV4::new(Ipv4Addr::UNSPECIFIED, config.port)).into(),
            )?;
            socket
        }
        (ipv4, true) => {
            let socket = Socket::new(Domain::IPV6, Type::DGRAM, Some(Protocol::UDP))?;
            socket.set_only_v6(!ipv4)?;
            socket.bind(
                &SocketAddr::V6(SocketAddrV6::new(Ipv6Addr::UNSPECIFIED, config.port, 0, 0)).into(),
            )?;
            socket
        }
        (false, false) => {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "neither IPv4 nor IPv6 is enabled",
            ));
        }
    };
    Ok(socket.into())
}

/// Binds the mDNS socket (port 5353, shared with other responders through `SO_REUSEADDR`) and
/// joins the mDNS multicast groups on `interface` for the enabled families.
///
/// Returns the socket and whether the IPv6 group was joined; a failed IPv6 join on an interface
/// without IPv6 is logged once and tolerated, as is a failed IPv4 join.
pub(crate) fn bind_mdns_socket(
    config: &NetworkConfig,
    interface: &InterfaceAddresses,
) -> io::Result<(UdpSocket, bool)> {
    let socket = Socket::new(Domain::IPV6, Type::DGRAM, Some(Protocol::UDP))?;
    socket.set_reuse_address(true)?;
    #[cfg(unix)]
    socket.set_reuse_port(true)?;
    socket.set_only_v6(!config.ipv4)?;
    socket
        .bind(&SocketAddr::V6(SocketAddrV6::new(Ipv6Addr::UNSPECIFIED, MDNS_PORT, 0, 0)).into())?;
    let socket: UdpSocket = socket.into();

    if let Some(ipv4) = interface.ipv4
        && let Err(error) = socket.join_multicast_v4(&MDNS_IPV4_BROADCAST_ADDR, &ipv4)
    {
        log::warn!(
            "mDNS: joining the IPv4 group on {} ({ipv4}) failed: {error}",
            interface.name
        );
    }
    let ipv6_joined = if interface.ipv6.is_empty() {
        log::info!(
            "mDNS: interface {} has no IPv6 address, announcing on IPv4 only",
            interface.name
        );
        false
    } else {
        match socket.join_multicast_v6(&MDNS_IPV6_BROADCAST_ADDR, interface.index) {
            Ok(()) => true,
            Err(error) => {
                log::warn!(
                    "mDNS: joining the IPv6 group on {} failed, announcing on IPv4 only: {error}",
                    interface.name
                );
                false
            }
        }
    };
    Ok((socket, ipv6_joined))
}

/// The mDNS host name of a node: 16 hexadecimal digits derived from its unique id, so that the
/// name is stable across restarts and distinct for distinct nodes.
pub(crate) fn hostname(unique_id: &str) -> String {
    // FNV-1a over the unique id; a hash, not a secret.
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for byte in unique_id.bytes() {
        hash ^= u64::from(byte);
        hash = hash.wrapping_mul(0x0100_0000_01b3);
    }
    format!("{hash:016X}")
}

#[cfg(test)]
mod tests {
    use std::net::{Ipv4Addr, Ipv6Addr};

    use matter_rs_engine::{Error, NetworkConfig};

    use super::{SystemInterface, bind_matter_socket, choose_interface, hostname};

    fn lan() -> SystemInterface {
        SystemInterface {
            name: "lan".into(),
            index: 7,
            loopback: false,
            ipv4: vec![Ipv4Addr::new(192, 168, 1, 10)],
            ipv6: vec![
                Ipv6Addr::new(0x2001, 0xdb8, 0, 0, 0, 0, 0, 1),
                Ipv6Addr::new(0xfe80, 0, 0, 0, 0, 0, 0, 1),
            ],
        }
    }

    fn v4_only() -> SystemInterface {
        SystemInterface {
            name: "eth".into(),
            index: 3,
            loopback: false,
            ipv4: vec![Ipv4Addr::new(10, 0, 0, 2)],
            ipv6: vec![],
        }
    }

    fn loopback() -> SystemInterface {
        SystemInterface {
            name: "lo".into(),
            index: 1,
            loopback: true,
            ipv4: vec![Ipv4Addr::LOCALHOST],
            ipv6: vec![Ipv6Addr::LOCALHOST],
        }
    }

    #[test]
    fn the_named_interface_wins() {
        let config = NetworkConfig {
            interface: Some("eth".into()),
            ..NetworkConfig::default()
        };
        let chosen = choose_interface(&config, &[loopback(), lan(), v4_only()]).unwrap();
        assert_eq!(chosen.name, "eth");
        assert_eq!(chosen.ipv4, Some(Ipv4Addr::new(10, 0, 0, 2)));
        assert!(chosen.ipv6.is_empty());

        let config = NetworkConfig {
            interface: Some("missing".into()),
            ..NetworkConfig::default()
        };
        assert!(matches!(
            choose_interface(&config, &[lan()]),
            Err(Error::InvalidIdentity { .. })
        ));
    }

    #[test]
    fn the_heuristic_prefers_ipv4_with_link_local_ipv6() {
        let chosen =
            choose_interface(&NetworkConfig::default(), &[loopback(), v4_only(), lan()]).unwrap();
        assert_eq!(chosen.name, "lan");
        assert_eq!(chosen.index, 7);
        // Link-local first, then the routable address.
        assert_eq!(
            chosen.ipv6,
            vec![
                Ipv6Addr::new(0xfe80, 0, 0, 0, 0, 0, 0, 1),
                Ipv6Addr::new(0x2001, 0xdb8, 0, 0, 0, 0, 0, 1)
            ]
        );

        let chosen = choose_interface(&NetworkConfig::default(), &[loopback(), v4_only()]).unwrap();
        assert_eq!(chosen.name, "eth");

        assert!(matches!(
            choose_interface(&NetworkConfig::default(), &[loopback()]),
            Err(Error::InvalidIdentity { .. })
        ));
    }

    #[test]
    fn disabled_families_are_left_out() {
        let config = NetworkConfig {
            ipv6: false,
            ..NetworkConfig::default()
        };
        let chosen = choose_interface(&config, &[lan()]).unwrap();
        assert!(chosen.ipv6.is_empty());
        assert!(chosen.ipv4.is_some());

        let config = NetworkConfig {
            ipv4: false,
            ..NetworkConfig::default()
        };
        assert!(matches!(
            choose_interface(&config, &[v4_only()]),
            Err(Error::InvalidIdentity { .. })
        ));
    }

    #[test]
    fn matter_socket_binds_for_each_family_set() {
        for (ipv4, ipv6) in [(true, true), (true, false), (false, true)] {
            let config = NetworkConfig {
                ipv4,
                ipv6,
                port: 0,
                ..NetworkConfig::default()
            };
            let socket = bind_matter_socket(&config).unwrap();
            let local = socket.local_addr().unwrap();
            assert_eq!(local.is_ipv6(), ipv6, "{ipv4} {ipv6}");
            assert_ne!(local.port(), 0);
        }
        let config = NetworkConfig {
            ipv4: false,
            ipv6: false,
            ..NetworkConfig::default()
        };
        assert!(bind_matter_socket(&config).is_err());
    }

    #[test]
    fn hostname_is_stable_hex() {
        let name = hostname("bridge-1");
        assert_eq!(name.len(), 16);
        assert!(name.chars().all(|c| c.is_ascii_hexdigit()));
        assert_eq!(name, hostname("bridge-1"));
        assert_ne!(name, hostname("bridge-2"));
    }
}
