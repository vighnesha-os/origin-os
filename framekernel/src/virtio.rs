//! virtio device negotiation and the virtio-net structures (virtio 1.1; CLAUDE.md §31
//! driver framework, §54 networking). This is the device-independent handshake (status
//! bits, feature negotiation) and the virtio-net wire structures (its config space and the
//! 12-byte packet header), as pure encoders/decoders. The kernel driver drives the real
//! MMIO/common-config with these values; the rings themselves are [`crate::virtqueue`].
//! Pure, no `unsafe`.

use crate::pci::{Bar, BarKind};

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

/// Where one virtio structure lives: a BAR index and a byte offset/length within it. All
/// three come from the device's capability; nothing else in the driver has checked them.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct CfgLocation {
    pub bar: u8,
    pub offset: u32,
    pub length: u32,
}

impl CfgLocation {
    /// The structure's physical address, if it lies wholly inside the memory BAR `bar` it
    /// names and is at least `min` bytes long (virtio 1.1 §4.1.4: "unexpectedly large" must
    /// be tolerated, too short and outside the BAR must not).
    pub fn address_in(&self, bar: &Bar, min: u32) -> Option<u64> {
        if bar.index != self.bar || bar.kind == BarKind::Io || self.length < min {
            return None;
        }
        let end = u64::from(self.offset).checked_add(u64::from(self.length))?;
        if end > bar.size {
            return None;
        }
        let address = bar.address.checked_add(u64::from(self.offset))?;
        address.checked_add(u64::from(self.length)).map(|_| address)
    }

    /// The offset of a queue's 16-bit notification register in this notification structure
    /// (`queue_notify_off × notify_off_multiplier`, virtio 1.1 §4.1.4.4), if it lies inside
    /// the structure: both factors are the device's.
    pub fn notify_offset(&self, queue_notify_off: u16, multiplier: u32) -> Option<u64> {
        let at = u64::from(queue_notify_off) * u64::from(multiplier);
        (at + 2 <= u64::from(self.length)).then_some(at)
    }
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

/// Bytes of a `virtio_pci_cap`, and of a `virtio_pci_notify_cap` (virtio 1.1 §4.1.4).
const CAP_LEN: u16 = 16;
const NOTIFY_CAP_LEN: u16 = 20;
/// Capabilities live in the first 256 bytes of configuration space.
const CAP_SPACE_END: u16 = 0x100;
/// BAR indexes a capability may name; the others are reserved and such a capability must be
/// ignored (virtio 1.1 §4.1.4.1).
const LAST_BAR: u8 = 5;

/// Walks the PCI capability list and collects the virtio modern capability structures.
/// `read` returns the little-endian dword at a byte offset in config space (offsets are
/// dword-aligned and below 100h). Bounded by the capability count so a corrupt `next` cannot
/// loop. A capability that does not fit in configuration space, is shorter than its structure,
/// names a reserved BAR or that `usable` turns down is passed over; of the rest, the first of
/// each type is taken (virtio 1.1 §4.1.4.1: "the first instance of each virtio structure type
/// they can support"; a later one is the device's fallback).
pub fn find_caps(
    mut read: impl FnMut(u16) -> u32,
    mut usable: impl FnMut(&CfgLocation) -> bool,
) -> Caps {
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
        let len = (d0 >> 16 & 0xFF) as u16;
        let kind = (d0 >> 24 & 0xFF) as u8;
        let needed = if kind == cfg::NOTIFY {
            NOTIFY_CAP_LEN
        } else {
            CAP_LEN
        };
        if vndr == cfg::VNDR && len >= needed && ptr + needed <= CAP_SPACE_END {
            let location = CfgLocation {
                bar: (read(ptr + 4) & 0xFF) as u8,
                offset: read(ptr + 8),
                length: read(ptr + 12),
            };
            if location.bar <= LAST_BAR && usable(&location) {
                let slot = match kind {
                    cfg::COMMON => Some(&mut caps.common),
                    cfg::NOTIFY => Some(&mut caps.notify),
                    cfg::ISR => Some(&mut caps.isr),
                    cfg::DEVICE => Some(&mut caps.device),
                    _ => None,
                };
                if let Some(slot) = slot.filter(|slot| slot.is_none()) {
                    *slot = Some(location);
                    if kind == cfg::NOTIFY {
                        caps.notify_off_multiplier = read(ptr + 16);
                    }
                }
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
        let caps = find_caps(read, |_| true);
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
        assert!(!find_caps(read2, |_| true).is_modern());
    }

    /// One 256-byte configuration space for the capability tests.
    fn config_space() -> [u8; 256] {
        let mut b = [0u8; 256];
        let mut put = |at: usize, v: u32| b[at..at + 4].copy_from_slice(&v.to_le_bytes());
        put(0x04, 0x0010_0000);
        put(0x34, 0x40);
        let cap = |next: u32, len: u32, ty: u32| 0x09 | next << 8 | len << 16 | ty << 24;
        put(0x40, cap(0x50, 16, 1));
        put(0x44, 4);
        put(0x48, 0);
        put(0x4C, 0x1000);
        put(0x50, cap(0x64, 20, 2));
        put(0x54, 4);
        put(0x58, 0x3000);
        put(0x5C, 0x1000);
        put(0x60, 4);
        put(0x64, cap(0x74, 16, 3));
        put(0x68, 4);
        put(0x6C, 0x1000);
        put(0x70, 0x1000);
        put(0x74, cap(0, 16, 4));
        put(0x78, 4);
        put(0x7C, 0x2000);
        put(0x80, 0x1000);
        b
    }

    fn reader(bytes: &[u8; 256]) -> impl FnMut(u16) -> u32 + '_ {
        |off| {
            let o = usize::from(off);
            assert!(off < 0x100 && off & 3 == 0, "read at {off:#x}");
            u32::from_le_bytes([bytes[o], bytes[o + 1], bytes[o + 2], bytes[o + 3]])
        }
    }

    /// The first usable instance of each structure wins, as virtio 1.1 §4.1.4.1 asks: a
    /// device may list an I/O-BAR notification structure before a memory-BAR fallback.
    #[test]
    fn the_first_usable_instance_of_a_structure_is_taken() {
        let mut b = config_space();
        // A second common structure, later in the list, at BAR 3.
        b[0x74..0x78].copy_from_slice(&(0x09u32 | 0x84 << 8 | 16 << 16 | 4 << 24).to_le_bytes());
        b[0x84..0x88].copy_from_slice(&(0x09u32 | 16 << 16 | 1 << 24).to_le_bytes());
        b[0x88..0x8C].copy_from_slice(&3u32.to_le_bytes());
        b[0x90..0x94].copy_from_slice(&0x1000u32.to_le_bytes());
        let caps = find_caps(reader(&b), |_| true);
        assert_eq!(caps.common.unwrap().bar, 4, "the first common structure");
        // With BAR 4 unusable, the later instance is the one the driver can take.
        let caps = find_caps(reader(&b), |l| l.bar != 4);
        assert_eq!(caps.common.unwrap().bar, 3, "the fallback");
        assert!(caps.notify.is_none(), "every other structure was at BAR 4");
    }

    #[test]
    fn capabilities_are_checked_before_their_bytes_are_used() {
        // A capability at the very top: its 16 bytes must end by 100h, so F0h is the last
        // place one fits, and at F4h it would cross into extended space.
        let mut b = config_space();
        b[0x34..0x38].copy_from_slice(&0xF0u32.to_le_bytes());
        b[0xF0..0xF4].copy_from_slice(&(0x09u32 | 16 << 16 | 1 << 24).to_le_bytes());
        assert!(find_caps(reader(&b), |_| true).common.is_some(), "F0h fits");
        b[0x34..0x38].copy_from_slice(&0xF4u32.to_le_bytes());
        b[0xF4..0xF8].copy_from_slice(&(0x09u32 | 16 << 16 | 1 << 24).to_le_bytes());
        assert!(
            find_caps(reader(&b), |_| true).common.is_none(),
            "F4h does not"
        );
        // A notify capability needs 20 bytes: at ECh it fits, at F0h it does not.
        let mut b = config_space();
        b[0x34..0x38].copy_from_slice(&0xECu32.to_le_bytes());
        b[0xEC..0xF0].copy_from_slice(&(0x09u32 | 20 << 16 | 2 << 24).to_le_bytes());
        assert!(find_caps(reader(&b), |_| true).notify.is_some(), "ECh fits");
        let mut b = config_space();
        b[0x34..0x38].copy_from_slice(&0xF0u32.to_le_bytes());
        b[0xF0..0xF4].copy_from_slice(&(0x09u32 | 20 << 16 | 2 << 24).to_le_bytes());
        assert!(
            find_caps(reader(&b), |_| true).notify.is_none(),
            "F0h does not"
        );
        // Too short for its structure, or a reserved BAR: ignored, the walk continues.
        let mut b = config_space();
        b[0x40..0x44].copy_from_slice(&(0x09u32 | 0x50 << 8 | 12 << 16 | 1 << 24).to_le_bytes());
        assert!(
            find_caps(reader(&b), |_| true).common.is_none(),
            "a 12-byte capability"
        );
        let mut b = config_space();
        b[0x44..0x48].copy_from_slice(&6u32.to_le_bytes());
        let caps = find_caps(reader(&b), |_| true);
        assert!(
            caps.common.is_none() && caps.notify.is_some(),
            "reserved BAR 6"
        );
    }

    #[test]
    fn a_structure_is_located_only_inside_its_memory_bar() {
        let loc = CfgLocation {
            bar: 4,
            offset: 0x1000,
            length: 0x1000,
        };
        let bar = Bar {
            index: 4,
            kind: BarKind::Memory64,
            prefetchable: true,
            address: 0xFE00_0000,
            size: 0x4000,
        };
        assert_eq!(loc.address_in(&bar, 56), Some(0xFE00_1000));
        assert_eq!(
            loc.address_in(&bar, 0x1001),
            None,
            "shorter than the driver needs"
        );
        let exact = Bar {
            size: 0x2000,
            ..bar
        };
        assert_eq!(
            loc.address_in(&exact, 1),
            Some(0xFE00_1000),
            "ends at the BAR's end"
        );
        let short = Bar {
            size: 0x1FFF,
            ..bar
        };
        assert_eq!(loc.address_in(&short, 1), None, "one byte past the BAR");
        assert_eq!(
            loc.address_in(
                &Bar {
                    kind: BarKind::Io,
                    ..bar
                },
                1
            ),
            None,
            "an I/O BAR is not memory"
        );
        assert_eq!(
            loc.address_in(&Bar { index: 0, ..bar }, 1),
            None,
            "the wrong BAR"
        );
        let wraps = Bar {
            address: u64::MAX - 0x17FF,
            ..bar
        };
        assert_eq!(loc.address_in(&wraps, 1), None, "wraps the address space");
    }

    #[test]
    fn a_notification_register_stays_inside_its_structure() {
        let notify = CfgLocation {
            bar: 4,
            offset: 0x3000,
            length: 0x1000,
        };
        assert_eq!(notify.notify_offset(1, 4), Some(4));
        assert_eq!(notify.notify_offset(1023, 4), Some(0xFFC));
        assert_eq!(notify.notify_offset(0, 0), Some(0), "a shared register");
        assert_eq!(notify.notify_offset(1024, 4), None, "past the structure");
        assert_eq!(
            notify.notify_offset(u16::MAX, u32::MAX),
            None,
            "no overflow"
        );
        let small = CfgLocation {
            length: 1,
            ..notify
        };
        assert_eq!(small.notify_offset(0, 0), None, "two bytes are written");
    }

    /// Whatever a virtio device's configuration space holds, and even if it changes
    /// between reads, the walk reads only inside the capability area and keeps what it
    /// promises (CLAUDE.md §72).
    #[test]
    fn hostile_capability_lists_never_break_the_walk() {
        use crate::testing::{Rng, mutate_window};
        let base = config_space();
        let mut rng = Rng::new(0x5649_5254);
        let (mut modern, mut seen) = (0u32, 0u32);
        for case in 0..crate::testing::cases(12_000) {
            let mut b = base;
            mutate_window(&mut b, 0x34..0x100, &mut rng);
            if case % 3 == 0 {
                // A capability at any offset the list can name, so the end of the area is
                // reached by the walk itself and not by a lucky mutation.
                let offset = 0x40 + 4 * rng.below(48);
                b[0x34] = offset as u8;
                let kind = 1 + rng.below(4) as u32;
                let len = if kind == 2 { 20 } else { 16 };
                let head = 0x09u32 | len << 16 | kind << 24;
                b[offset..offset + 4].copy_from_slice(&head.to_le_bytes());
            }
            let noisy = case % 4 == 0;
            let mut noise = Rng::new(rng.next());
            let mut reads = 0u32;
            let caps = find_caps(
                |off| {
                    reads += 1;
                    assert!(off < 0x100 && off & 3 == 0, "case {case}: read at {off:#x}");
                    let o = usize::from(off);
                    let v = u32::from_le_bytes([b[o], b[o + 1], b[o + 2], b[o + 3]]);
                    if noisy { v ^ noise.edge_u32() } else { v }
                },
                |l| l.bar != 2,
            );
            assert!(reads <= 2 + 48 * 5, "case {case}: {reads} reads");
            modern += u32::from(caps.is_modern());
            for loc in [caps.common, caps.notify, caps.isr, caps.device]
                .into_iter()
                .flatten()
            {
                seen += 1;
                assert!(loc.bar <= 5 && loc.bar != 2, "case {case}: {loc:?}");
            }
            assert!(
                caps.notify.is_some() || caps.notify_off_multiplier == 0,
                "case {case}"
            );

            let mut plain = base;
            mutate_window(&mut plain, 0x34..0x100, &mut rng);
            let steady = find_caps(reader(&plain), |_| true);
            // A steady device's locations are what its capability bytes say.
            for loc in [steady.common, steady.notify, steady.isr, steady.device]
                .into_iter()
                .flatten()
            {
                let bar = Bar {
                    index: loc.bar,
                    kind: BarKind::Memory32,
                    prefetchable: false,
                    address: u64::from(rng.edge_u32()),
                    size: 1 << rng.below(32),
                };
                if let Some(address) = loc.address_in(&bar, 2) {
                    assert!(
                        u64::from(loc.offset) + u64::from(loc.length) <= bar.size,
                        "case {case}: {loc:?}"
                    );
                    assert_eq!(address, bar.address + u64::from(loc.offset), "case {case}");
                }
                if let Some(at) = loc.notify_offset(rng.next() as u16, rng.edge_u32()) {
                    assert!(at + 2 <= u64::from(loc.length), "case {case}");
                }
            }
        }
        assert!(
            modern > 1000 && seen > 1000,
            "{modern} modern, {seen} structures"
        );
    }

    /// The virtio-net structures decode exactly the bytes they cover; a header round-trips
    /// and a configuration field is present only with its feature (CLAUDE.md §72).
    #[test]
    fn mutated_net_structures_decode_exactly_their_bytes() {
        use crate::testing::{Rng, mutate};
        let mut rng = Rng::new(0x4E45_5456);
        for case in 0..crate::testing::cases(10_000) {
            let hdr = NetHeader {
                flags: rng.byte(),
                gso_type: rng.byte(),
                hdr_len: rng.edge_u32() as u16,
                gso_size: rng.edge_u32() as u16,
                csum_start: rng.edge_u32() as u16,
                csum_offset: rng.edge_u32() as u16,
                num_buffers: rng.edge_u32() as u16,
            };
            let bytes = hdr.encode();
            assert_eq!(NetHeader::decode(&bytes), Some(hdr), "case {case}");
            let mut mutated = bytes.to_vec();
            mutate(&mut mutated, &mut rng);
            match NetHeader::decode(&mutated) {
                Some(h) => {
                    assert!(mutated.len() >= NET_HDR_LEN, "case {case}");
                    assert_eq!(h.encode()[..], mutated[..NET_HDR_LEN], "case {case}");
                }
                None => assert!(mutated.len() < NET_HDR_LEN, "case {case}"),
            }

            let mut config = [0u8; 12].to_vec();
            for b in &mut config {
                *b = rng.byte();
            }
            let features = rng.next();
            let mut cut = config;
            mutate(&mut cut, &mut rng);
            let c = parse_net_config(&cut, features);
            assert_eq!(
                c.mac.is_some(),
                features & feature::NET_MAC != 0 && cut.len() >= 6
            );
            assert_eq!(
                c.status.is_some(),
                features & feature::NET_STATUS != 0 && cut.len() >= 8,
                "case {case}"
            );
            assert_eq!(
                c.max_queue_pairs.is_some(),
                features & feature::NET_MQ != 0 && cut.len() >= 10,
                "case {case}"
            );
            assert_eq!(
                c.mtu.is_some(),
                features & feature::NET_MTU != 0 && cut.len() >= 12,
                "case {case}"
            );
            if let Some(mac) = c.mac {
                assert_eq!(mac[..], cut[..6], "case {case}");
            }
            assert_eq!(
                link_up(&c),
                c.status.is_some_and(|s| s & 1 == 1),
                "case {case}"
            );

            let (offered, wanted) = (rng.next(), rng.next());
            if let Some(accepted) = negotiate(offered, wanted) {
                assert_eq!(accepted, offered & wanted, "case {case}");
                assert_ne!(accepted & feature::VERSION_1, 0, "case {case}");
            } else {
                assert_eq!(offered & wanted & feature::VERSION_1, 0, "case {case}");
            }
        }
    }
}
