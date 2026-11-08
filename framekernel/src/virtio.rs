//! virtio device negotiation and the virtio-net structures (virtio 1.1; CLAUDE.md §31
//! driver framework, §54 networking). This is the device-independent handshake (status
//! bits, feature negotiation) and the virtio-net wire structures (its config space and the
//! 12-byte packet header), as pure encoders/decoders. The kernel driver drives the real
//! MMIO/common-config with these values; the rings themselves are [`crate::virtqueue`].
//! Pure, no `unsafe`.

/// Device status bits written to the common config (virtio 1.1 §2.1).
pub mod status {
    pub const ACKNOWLEDGE: u8 = 1;
    pub const DRIVER: u8 = 2;
    pub const DRIVER_OK: u8 = 4;
    pub const FEATURES_OK: u8 = 8;
    pub const DEVICE_NEEDS_RESET: u8 = 64;
    pub const FAILED: u8 = 128;
}

/// Feature bits (transport bit 32, plus the virtio-net features this driver understands).
pub mod feature {
    /// The device speaks virtio 1.0+ (required; this driver refuses legacy-only devices).
    pub const VERSION_1: u64 = 1 << 32;
    pub const NET_CSUM: u64 = 1 << 0;
    pub const NET_MTU: u64 = 1 << 3;
    pub const NET_MAC: u64 = 1 << 5;
    pub const NET_MRG_RXBUF: u64 = 1 << 15;
    pub const NET_STATUS: u64 = 1 << 16;
    pub const NET_MQ: u64 = 1 << 22;
}

/// Negotiates features: the driver keeps only what both it and the device offer, and fails
/// (returns `None`) unless the result includes `VERSION_1` — this driver does not support
/// legacy devices. `device` is what the device offered; `wanted` is what the driver asked.
pub fn negotiate(device: u64, wanted: u64) -> Option<u64> {
    let accepted = device & wanted;
    (accepted & feature::VERSION_1 != 0).then_some(accepted)
}

/// The virtio-net configuration space (virtio 1.1 §5.1.4), parsed per the accepted features.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct NetConfig {
    pub mac: Option<[u8; 6]>,
    pub status: Option<u16>,
    pub max_queue_pairs: Option<u16>,
    pub mtu: Option<u16>,
}

fn le16(b: &[u8], at: usize) -> Option<u16> {
    Some(u16::from_le_bytes([*b.get(at)?, *b.get(at + 1)?]))
}

/// Reads the virtio-net config structure, taking each field only when its feature was
/// accepted. A field whose bytes are missing is left `None` rather than panicking.
pub fn parse_net_config(config: &[u8], features: u64) -> NetConfig {
    let mut c = NetConfig::default();
    if features & feature::NET_MAC != 0
        && let Some(mac) = config.get(0..6)
    {
        c.mac = Some([mac[0], mac[1], mac[2], mac[3], mac[4], mac[5]]);
    }
    if features & feature::NET_STATUS != 0 {
        c.status = le16(config, 6);
    }
    if features & feature::NET_MQ != 0 {
        c.max_queue_pairs = le16(config, 8);
    }
    if features & feature::NET_MTU != 0 {
        c.mtu = le16(config, 10);
    }
    c
}

/// Whether the device-reported link is up (`VIRTIO_NET_S_LINK_UP`), when `NET_STATUS` was
/// negotiated.
pub fn link_up(config: &NetConfig) -> bool {
    config.status.is_some_and(|s| s & 1 != 0)
}

/// The length of the virtio-net packet header (modern layout, with `num_buffers`).
pub const NET_HDR_LEN: usize = 12;

/// The virtio-net packet header (virtio 1.1 §5.1.6), prepended to every frame on the wire.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct NetHeader {
    pub flags: u8,
    pub gso_type: u8,
    pub hdr_len: u16,
    pub gso_size: u16,
    pub csum_start: u16,
    pub csum_offset: u16,
    /// The number of merged receive buffers (1 for a single-buffer frame).
    pub num_buffers: u16,
}

impl NetHeader {
    /// A plain header for a frame with no offloads (the common transmit case).
    pub fn plain() -> NetHeader {
        NetHeader {
            num_buffers: 1,
            ..NetHeader::default()
        }
    }

    pub fn encode(&self) -> [u8; NET_HDR_LEN] {
        let mut b = [0u8; NET_HDR_LEN];
        b[0] = self.flags;
        b[1] = self.gso_type;
        b[2..4].copy_from_slice(&self.hdr_len.to_le_bytes());
        b[4..6].copy_from_slice(&self.gso_size.to_le_bytes());
        b[6..8].copy_from_slice(&self.csum_start.to_le_bytes());
        b[8..10].copy_from_slice(&self.csum_offset.to_le_bytes());
        b[10..12].copy_from_slice(&self.num_buffers.to_le_bytes());
        b
    }

    /// Decodes a header from the start of `bytes`, or `None` if it is too short.
    pub fn decode(bytes: &[u8]) -> Option<NetHeader> {
        if bytes.len() < NET_HDR_LEN {
            return None;
        }
        Some(NetHeader {
            flags: bytes[0],
            gso_type: bytes[1],
            hdr_len: le16(bytes, 2)?,
            gso_size: le16(bytes, 4)?,
            csum_start: le16(bytes, 6)?,
            csum_offset: le16(bytes, 8)?,
            num_buffers: le16(bytes, 10)?,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn feature_negotiation_requires_version_1() {
        let device =
            feature::VERSION_1 | feature::NET_MAC | feature::NET_STATUS | feature::NET_CSUM;
        let wanted = feature::VERSION_1 | feature::NET_MAC | feature::NET_STATUS;
        // The accepted set is the intersection.
        assert_eq!(
            negotiate(device, wanted),
            Some(feature::VERSION_1 | feature::NET_MAC | feature::NET_STATUS)
        );
        // A legacy-only device (no VERSION_1) is refused.
        assert_eq!(
            negotiate(feature::NET_MAC, feature::NET_MAC | feature::VERSION_1),
            None
        );
        // The driver never accepts a feature it did not ask for.
        let acc = negotiate(device, feature::VERSION_1).unwrap();
        assert_eq!(acc & feature::NET_CSUM, 0);
    }

    #[test]
    fn net_config_is_read_per_feature() {
        // mac, then status=LINK_UP(1), then (no MQ), then mtu=1500.
        let mut cfg = [0u8; 12];
        cfg[0..6].copy_from_slice(&[0x52, 0x54, 0x00, 0x12, 0x34, 0x56]);
        cfg[6..8].copy_from_slice(&1u16.to_le_bytes());
        cfg[10..12].copy_from_slice(&1500u16.to_le_bytes());

        let feats = feature::VERSION_1 | feature::NET_MAC | feature::NET_STATUS | feature::NET_MTU;
        let c = parse_net_config(&cfg, feats);
        assert_eq!(c.mac, Some([0x52, 0x54, 0x00, 0x12, 0x34, 0x56]));
        assert_eq!(c.status, Some(1));
        assert!(link_up(&c));
        assert_eq!(c.mtu, Some(1500));
        assert_eq!(c.max_queue_pairs, None, "MQ not negotiated");

        // Without NET_STATUS/NET_MTU, those fields are not read even though bytes exist.
        let c2 = parse_net_config(&cfg, feature::VERSION_1 | feature::NET_MAC);
        assert_eq!(c2.mac, c.mac);
        assert_eq!((c2.status, c2.mtu), (None, None));
        assert!(!link_up(&c2));

        // A truncated config does not panic.
        let c3 = parse_net_config(&[0x52, 0x54], feats);
        assert_eq!(c3.mac, None, "6 mac bytes not present");
    }

    #[test]
    fn net_header_round_trips() {
        let h = NetHeader::plain();
        assert_eq!(h.num_buffers, 1);
        assert_eq!(NetHeader::decode(&h.encode()), Some(h));
        let fancy = NetHeader {
            flags: 1,
            gso_type: 1,
            hdr_len: 14,
            gso_size: 1460,
            csum_start: 34,
            csum_offset: 6,
            num_buffers: 3,
        };
        let bytes = fancy.encode();
        assert_eq!(bytes.len(), NET_HDR_LEN);
        assert_eq!(NetHeader::decode(&bytes), Some(fancy));
        assert_eq!(NetHeader::decode(&[0u8; 11]), None, "too short");
    }

    #[test]
    fn status_bits_are_distinct() {
        // The driver's bring-up progression ORs these in turn; they must not overlap.
        let all = status::ACKNOWLEDGE | status::DRIVER | status::FEATURES_OK | status::DRIVER_OK;
        assert_eq!(all.count_ones(), 4);
    }
}
