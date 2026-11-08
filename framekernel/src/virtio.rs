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
    /// The device's DMA goes through the platform's IOMMU, so the driver gives it bus
    /// addresses (IOVAs) rather than physical ones (virtio 1.1 §6, bit 33). A driver must
    /// accept it when offered, or the device may refuse `FEATURES_OK`.
    pub const ACCESS_PLATFORM: u64 = 1 << 33;
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

/// A virtio PCI capability `cfg_type` (virtio 1.1 §4.1.4).
pub mod cfg {
    pub const VNDR: u8 = 0x09; // PCI vendor-specific capability id
    pub const COMMON: u8 = 1;
    pub const NOTIFY: u8 = 2;
    pub const ISR: u8 = 3;
    pub const DEVICE: u8 = 4;
}

/// Where one virtio structure lives: a BAR index and a byte offset/length within it.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct CfgLocation {
    pub bar: u8,
    pub offset: u32,
    pub length: u32,
}

/// The virtio modern capabilities a driver needs, discovered from PCI config space.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Caps {
    pub common: Option<CfgLocation>,
    pub notify: Option<CfgLocation>,
    pub isr: Option<CfgLocation>,
    pub device: Option<CfgLocation>,
    /// The notify capability's `notify_off_multiplier`.
    pub notify_off_multiplier: u32,
}

impl Caps {
    /// Whether the mandatory structures for a modern driver were found.
    pub fn is_modern(&self) -> bool {
        self.common.is_some() && self.notify.is_some() && self.device.is_some()
    }
}

/// Walks the PCI capability list and collects the virtio modern capability structures.
/// `read` returns the little-endian dword at a byte offset in config space (offsets are
/// dword-aligned here). Bounded by the capability count so a corrupt `next` cannot loop.
pub fn find_caps(mut read: impl FnMut(u16) -> u32) -> Caps {
    let mut caps = Caps::default();
    // Capabilities exist only if the status register (offset 0x06) bit 4 is set.
    if read(0x04) >> 16 & (1 << 4) == 0 {
        return caps;
    }
    let mut ptr = (read(0x34) & 0xFF) as u16; // capabilities pointer
    for _ in 0..48 {
        if ptr < 0x40 || ptr & 0x3 != 0 {
            break; // a valid capability lies past the header, dword-aligned
        }
        let d0 = read(ptr); // cap_vndr | cap_next<<8 | cap_len<<16 | cfg_type<<24
        let vndr = (d0 & 0xFF) as u8;
        let next = (d0 >> 8 & 0xFF) as u16;
        if vndr == cfg::VNDR {
            let bar = (read(ptr + 4) & 0xFF) as u8;
            let location = CfgLocation {
                bar,
                offset: read(ptr + 8),
                length: read(ptr + 12),
            };
            match (d0 >> 24 & 0xFF) as u8 {
                cfg::COMMON => caps.common = Some(location),
                cfg::NOTIFY => {
                    caps.notify = Some(location);
                    caps.notify_off_multiplier = read(ptr + 16);
                }
                cfg::ISR => caps.isr = Some(location),
                cfg::DEVICE => caps.device = Some(location),
                _ => {}
            }
        }
        if next == 0 {
            break;
        }
        ptr = next;
    }
    caps
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
        // ACCESS_PLATFORM (bit 33, in the high feature word) is kept when the device offers
        // it, and absent when it does not: the driver then gives physical addresses.
        let behind_iommu = device | feature::ACCESS_PLATFORM;
        let wanted = wanted | feature::ACCESS_PLATFORM;
        let acc = negotiate(behind_iommu, wanted).unwrap();
        assert_ne!(acc & feature::ACCESS_PLATFORM, 0);
        assert_eq!((acc >> 32) as u32, 0b11, "VERSION_1 and ACCESS_PLATFORM");
        assert_eq!(
            negotiate(device, wanted).unwrap() & feature::ACCESS_PLATFORM,
            0
        );
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

    #[test]
    fn find_caps_discovers_the_virtio_structures() {
        // A 256-byte config space with a capability list of the four virtio caps.
        let mut cfg = [0u8; 256];
        let mut put = |at: usize, v: u32| cfg[at..at + 4].copy_from_slice(&v.to_le_bytes());
        put(0x04, 0x0010_0000); // status (high word) with the capabilities bit (4) set
        put(0x34, 0x40); // capabilities pointer
        let cap = |vndr: u32, next: u32, len: u32, ty: u32| vndr | next << 8 | len << 16 | ty << 24;
        // common @0x40 → BAR 4, offset 0
        put(0x40, cap(cfg::VNDR.into(), 0x50, 16, cfg::COMMON.into()));
        put(0x44, 4);
        put(0x48, 0);
        put(0x4C, 0x1000);
        // notify @0x50 (20 bytes) → offset 0x3000, multiplier 4 at +16
        put(0x50, cap(cfg::VNDR.into(), 0x64, 20, cfg::NOTIFY.into()));
        put(0x54, 4);
        put(0x58, 0x3000);
        put(0x5C, 0x1000);
        put(0x60, 4);
        // isr @0x64 → offset 0x1000
        put(0x64, cap(cfg::VNDR.into(), 0x74, 16, cfg::ISR.into()));
        put(0x68, 4);
        put(0x6C, 0x1000);
        put(0x70, 0x1000);
        // device @0x74 → offset 0x2000, end of list
        put(0x74, cap(cfg::VNDR.into(), 0, 16, cfg::DEVICE.into()));
        put(0x78, 4);
        put(0x7C, 0x2000);
        put(0x80, 0x1000);

        let read = |off: u16| {
            let o = off as usize;
            u32::from_le_bytes([cfg[o], cfg[o + 1], cfg[o + 2], cfg[o + 3]])
        };
        let caps = find_caps(read);
        assert!(caps.is_modern());
        assert_eq!(
            caps.common,
            Some(CfgLocation {
                bar: 4,
                offset: 0,
                length: 0x1000
            })
        );
        assert_eq!(caps.notify.unwrap().offset, 0x3000);
        assert_eq!(caps.notify_off_multiplier, 4);
        assert_eq!(caps.isr.unwrap().offset, 0x1000);
        assert_eq!(caps.device.unwrap().offset, 0x2000);

        // With the capabilities bit clear, nothing is discovered.
        let mut cfg2 = cfg;
        cfg2[4..8].copy_from_slice(&0u32.to_le_bytes());
        let read2 = |off: u16| {
            let o = off as usize;
            u32::from_le_bytes([cfg2[o], cfg2[o + 1], cfg2[o + 2], cfg2[o + 3]])
        };
        assert!(!find_caps(read2).is_modern());
    }
}
