//! Finds IP/network cameras on the LAN via WS-Discovery (the multicast
//! probe/match protocol ONVIF cameras use), so a user can find a
//! camera's address without already knowing it.
//!
//! Deliberately stops at "here are the device service URLs (XAddrs) and
//! IP addresses that answered" - going further (calling the ONVIF device
//! service to ask a camera for its actual RTSP stream URI) needs
//! per-vendor credentials up front, which this project doesn't have at
//! discovery time. The user still has to fill in the RTSP path and any
//! camera credentials themselves in "+ Add camera", same as before; this
//! just saves them from having to find the IP address (checking a router's
//! DHCP client list, or a network scanner) first.

use std::net::{Ipv4Addr, SocketAddr, SocketAddrV4};
use std::time::Duration;

use serde::Serialize;
use tokio::net::UdpSocket;

const WS_DISCOVERY_MULTICAST_ADDR: Ipv4Addr = Ipv4Addr::new(239, 255, 255, 250);
const WS_DISCOVERY_PORT: u16 = 3702;

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct DiscoveredDevice {
    /// The IP address that answered the probe.
    pub address: String,
    /// ONVIF device service URL(s) (XAddrs) advertised in the probe
    /// match - typically `http://<ip>/onvif/device_service`, from which
    /// a compatible client could go on to ask the camera for its RTSP
    /// stream URI, though this project doesn't do that step (see module
    /// docs).
    pub xaddrs: Vec<String>,
}

/// Sends a WS-Discovery `Probe` for `NetworkVideoTransmitter` devices
/// (the ONVIF device class for cameras) to the standard discovery
/// multicast group, and collects `ProbeMatch` replies for `timeout`.
/// Returns whatever answered, deduplicated by address - an empty result
/// just means no ONVIF camera on this network segment responded in time
/// (multicast doesn't cross routers, so this only ever finds cameras on
/// the same LAN segment as the server).
pub async fn discover(timeout: Duration) -> anyhow::Result<Vec<DiscoveredDevice>> {
    let socket = UdpSocket::bind((Ipv4Addr::UNSPECIFIED, 0)).await?;
    socket.join_multicast_v4(WS_DISCOVERY_MULTICAST_ADDR, Ipv4Addr::UNSPECIFIED)?;

    let probe = build_probe();
    let dest = SocketAddr::V4(SocketAddrV4::new(WS_DISCOVERY_MULTICAST_ADDR, WS_DISCOVERY_PORT));
    socket.send_to(probe.as_bytes(), dest).await?;

    let mut found: Vec<DiscoveredDevice> = Vec::new();
    let mut buf = vec![0u8; 65535];
    let deadline = tokio::time::Instant::now() + timeout;

    loop {
        let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
        if remaining.is_zero() {
            break;
        }
        let (len, from) = match tokio::time::timeout(remaining, socket.recv_from(&mut buf)).await
        {
            Ok(Ok(pair)) => pair,
            Ok(Err(err)) => {
                tracing::debug!(%err, "WS-Discovery socket read failed");
                break;
            }
            Err(_) => break, // overall timeout elapsed
        };

        let address = from.ip().to_string();
        if found.iter().any(|d: &DiscoveredDevice| d.address == address) {
            continue;
        }
        let body = String::from_utf8_lossy(&buf[..len]);
        let xaddrs = extract_xaddrs(&body);
        found.push(DiscoveredDevice { address, xaddrs });
    }

    Ok(found)
}

fn build_probe() -> String {
    let message_id = uuid::Uuid::new_v4();
    format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<e:Envelope xmlns:e="http://www.w3.org/2003/05/soap-envelope"
            xmlns:w="http://schemas.xmlsoap.org/ws/2004/08/addressing"
            xmlns:d="http://schemas.xmlsoap.org/ws/2005/04/discovery"
            xmlns:dn="http://www.onvif.org/ver10/network/wsdl">
  <e:Header>
    <w:MessageID>uuid:{message_id}</w:MessageID>
    <w:To e:mustUnderstand="1">urn:schemas-xmlsoap-org:ws:2005:04:discovery</w:To>
    <w:Action e:mustUnderstand="1">http://schemas.xmlsoap.org/ws/2005/04/discovery/Probe</w:Action>
  </e:Header>
  <e:Body>
    <d:Probe>
      <d:Types>dn:NetworkVideoTransmitter</d:Types>
    </d:Probe>
  </e:Body>
</e:Envelope>"#
    )
}

/// Pulls `XAddrs` values out of a WS-Discovery reply without a full XML
/// parser - real-world responses use varying namespace prefixes
/// (`d:XAddrs`, `wsdd:XAddrs`, `a:XAddrs`, ...) so this matches on the
/// unprefixed local name instead of trying to enumerate every prefix a
/// camera vendor's stack might pick.
fn extract_xaddrs(body: &str) -> Vec<String> {
    let Some(start_tag) = body.find("XAddrs>") else {
        return Vec::new();
    };
    let content_start = start_tag + "XAddrs>".len();
    let Some(end_offset) = body[content_start..].find('<') else {
        return Vec::new();
    };
    body[content_start..content_start + end_offset]
        .split_whitespace()
        .map(|s| s.to_string())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extracts_single_xaddr_regardless_of_prefix() {
        let body = r#"<d:ProbeMatch><d:XAddrs>http://192.168.1.50/onvif/device_service</d:XAddrs></d:ProbeMatch>"#;
        assert_eq!(
            extract_xaddrs(body),
            vec!["http://192.168.1.50/onvif/device_service"]
        );
    }

    #[test]
    fn extracts_multiple_space_separated_xaddrs() {
        let body = r#"<wsdd:XAddrs>http://192.168.1.50/onvif/device_service http://[fe80::1]/onvif/device_service</wsdd:XAddrs>"#;
        assert_eq!(
            extract_xaddrs(body),
            vec![
                "http://192.168.1.50/onvif/device_service",
                "http://[fe80::1]/onvif/device_service",
            ]
        );
    }

    #[test]
    fn missing_xaddrs_returns_empty() {
        assert_eq!(extract_xaddrs("<d:ProbeMatch></d:ProbeMatch>"), Vec::<String>::new());
    }
}
