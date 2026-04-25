use std::{
    fmt, io,
    sync::{
        atomic::{AtomicUsize, Ordering},
        Arc, OnceLock,
    },
};

use crate::core::error;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum GsoType {
    None,
    TcpV4,
    TcpV6,
    UdpL4,
    Other(u8),
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct OffloadInfo {
    pub gso_type: GsoType,
    pub gso_size: u16,
    pub hdr_len: u16,
    pub csum_start: u16,
    pub csum_offset: u16,
    pub needs_csum: bool,
}

pub(crate) trait PacketRecycle: Send + Sync {
    fn recycle(&self);
}

pub struct Packet {
    bytes: PacketBytes,
    recycle: Option<Arc<dyn PacketRecycle>>,
    view: PacketView,
}

impl Packet {
    pub fn as_bytes(&self) -> &[u8] {
        self.bytes.raw_bytes()
    }

    pub fn len(&self) -> usize {
        self.bytes.len()
    }

    pub fn is_detached(&self) -> bool {
        self.recycle.is_none()
    }

    pub fn detach(&mut self) {
        self.bytes.detach();
        self.recycle_now();
    }

    pub fn offload_info(&self) -> Option<&OffloadInfo> {
        self.view.offload_info(&self.bytes)
    }

    pub fn is_gso(&self) -> bool {
        self.offload_info()
            .is_some_and(|info| info.gso_type != GsoType::None && info.gso_size > 0)
    }

    pub fn split_into<B: AsMut<[u8]>>(
        &self,
        out: &mut [B],
        sizes: &mut [usize],
        offset: usize,
    ) -> io::Result<usize> {
        match self.offload_info() {
            Some(info) => {
                let payload = &self.as_bytes()[VIRTIO_NET_HDR_LEN..];
                if info.gso_type == GsoType::None || info.gso_size == 0 {
                    if info.needs_csum {
                        let mut copy = payload.to_vec();
                        validate_checksum_bounds(copy.len(), info.csum_start, info.csum_offset)?;
                        gso_none_checksum(&mut copy, info.csum_start, info.csum_offset);
                        return copy_single_segment(&copy, out, sizes, offset);
                    }
                    return copy_single_segment(payload, out, sizes, offset);
                }

                let mut copy = payload.to_vec();
                split_gso_packet(&mut copy, info, out, sizes, offset)
            }
            _ => Err(error::unsupported("no offload info")),
        }
    }

    #[cfg_attr(not(test), allow(dead_code))]
    pub(crate) fn from_owned(bytes: Vec<u8>) -> Self {
        Self {
            bytes: PacketBytes::Owned(bytes),
            recycle: None,
            view: PacketView::plain(None),
        }
    }

    #[cfg_attr(not(test), allow(dead_code))]
    pub(crate) fn from_recyclable(
        bytes: Vec<u8>,
        recycle: RecycleToken,
        offload_info: Option<OffloadInfo>,
    ) -> Self {
        Self {
            bytes: PacketBytes::Owned(bytes),
            recycle: Some(Arc::new(recycle)),
            view: PacketView::plain(offload_info),
        }
    }

    pub(crate) fn from_ring(
        addr: usize,
        len: usize,
        recycle: Arc<dyn PacketRecycle>,
        offload_info: Option<OffloadInfo>,
    ) -> Self {
        Self {
            bytes: PacketBytes::Borrowed { addr, len },
            recycle: Some(recycle),
            view: PacketView::plain(offload_info),
        }
    }

    pub(crate) fn from_ring_with_virtio_net_hdr(
        addr: usize,
        len: usize,
        recycle: Arc<dyn PacketRecycle>,
    ) -> Self {
        Self {
            bytes: PacketBytes::Borrowed { addr, len },
            recycle: Some(recycle),
            view: PacketView::virtio_net_hdr(),
        }
    }

    #[cfg_attr(not(test), allow(dead_code))]
    pub(crate) fn from_recyclable_with_virtio_net_hdr(
        bytes: Vec<u8>,
        recycle: RecycleToken,
    ) -> Self {
        Self {
            bytes: PacketBytes::Owned(bytes),
            recycle: Some(Arc::new(recycle)),
            view: PacketView::virtio_net_hdr(),
        }
    }

    fn recycle_now(&mut self) {
        if let Some(recycle) = self.recycle.take() {
            recycle.recycle();
        }
    }
}

impl fmt::Debug for Packet {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Packet")
            .field("len", &self.len())
            .field("is_detached", &self.is_detached())
            .field("offload_info", &self.offload_info())
            .finish()
    }
}

struct PacketView {
    payload_offset: usize,
    offload_info: OnceLock<Option<OffloadInfo>>,
}

impl PacketView {
    fn plain(offload_info: Option<OffloadInfo>) -> Self {
        let cell = OnceLock::new();
        if let Some(info) = offload_info {
            let _ = cell.set(Some(info));
        }
        Self {
            payload_offset: 0,
            offload_info: cell,
        }
    }

    fn virtio_net_hdr() -> Self {
        Self {
            payload_offset: VIRTIO_NET_HDR_LEN,
            offload_info: OnceLock::new(),
        }
    }

    fn offload_info<'a>(&'a self, bytes: &'a PacketBytes) -> Option<&'a OffloadInfo> {
        self.offload_info
            .get_or_init(|| self.parse_offload_info(bytes))
            .as_ref()
    }

    fn parse_offload_info(&self, bytes: &PacketBytes) -> Option<OffloadInfo> {
        if self.payload_offset != VIRTIO_NET_HDR_LEN {
            return None;
        }

        let raw = bytes.raw_bytes();
        if raw.len() < VIRTIO_NET_HDR_LEN {
            return None;
        }

        let flags = raw[0];
        let raw_gso_type = raw[1];

        Some(OffloadInfo {
            gso_type: parse_gso_type(raw_gso_type),
            hdr_len: u16::from_ne_bytes([raw[2], raw[3]]),
            gso_size: u16::from_ne_bytes([raw[4], raw[5]]),
            csum_start: u16::from_ne_bytes([raw[6], raw[7]]),
            csum_offset: u16::from_ne_bytes([raw[8], raw[9]]),
            needs_csum: flags & VIRTIO_NET_HDR_F_NEEDS_CSUM != 0,
        })
    }
}

impl Drop for Packet {
    fn drop(&mut self) {
        self.recycle_now();
    }
}

enum PacketBytes {
    Owned(Vec<u8>),
    Borrowed { addr: usize, len: usize },
}

impl PacketBytes {
    fn raw_bytes(&self) -> &[u8] {
        match self {
            Self::Owned(bytes) => bytes,
            Self::Borrowed { addr, len } => unsafe {
                std::slice::from_raw_parts((*addr) as *const u8, *len)
            },
        }
    }

    fn len(&self) -> usize {
        match self {
            Self::Owned(bytes) => bytes.len(),
            Self::Borrowed { len, .. } => *len,
        }
    }

    fn detach(&mut self) {
        if matches!(self, Self::Borrowed { .. }) {
            *self = Self::Owned(self.raw_bytes().to_vec());
        }
    }
}

#[derive(Clone, Debug, Default)]
#[allow(dead_code)]
pub(crate) struct RecycleCounter {
    count: Arc<AtomicUsize>,
}

#[allow(dead_code)]
impl RecycleCounter {
    pub(crate) fn new() -> Self {
        Self::default()
    }

    pub(crate) fn token(&self) -> RecycleToken {
        RecycleToken {
            count: Arc::clone(&self.count),
        }
    }

    pub(crate) fn load(&self) -> usize {
        self.count.load(Ordering::SeqCst)
    }
}

#[derive(Debug)]
#[allow(dead_code)]
pub(crate) struct RecycleToken {
    count: Arc<AtomicUsize>,
}

impl PacketRecycle for RecycleToken {
    fn recycle(&self) {
        self.count.fetch_add(1, Ordering::SeqCst);
    }
}

const IPV4_SRC_ADDR_OFFSET: usize = 12;
const IPV6_SRC_ADDR_OFFSET: usize = 8;
const VIRTIO_NET_HDR_LEN: usize = tun_rs::VIRTIO_NET_HDR_LEN;
const VIRTIO_NET_HDR_F_NEEDS_CSUM: u8 = 0x01;
const VIRTIO_NET_HDR_GSO_ECN: u8 = 0x80;
const UDP_HEADER_LEN: usize = 8;
const TCP_FLAGS_OFFSET: usize = 13;
const TCP_FLAG_FIN: u8 = 0x01;
const TCP_FLAG_PSH: u8 = 0x08;

fn parse_gso_type(raw_gso_type: u8) -> GsoType {
    match raw_gso_type & !VIRTIO_NET_HDR_GSO_ECN {
        0 => GsoType::None,
        1 => GsoType::TcpV4,
        4 => GsoType::TcpV6,
        5 => GsoType::UdpL4,
        _ => GsoType::Other(raw_gso_type),
    }
}

fn copy_single_segment<B: AsMut<[u8]>>(
    packet: &[u8],
    out: &mut [B],
    sizes: &mut [usize],
    offset: usize,
) -> io::Result<usize> {
    ensure_output_slots(out.len(), sizes.len(), 1)?;

    let required_len = offset
        .checked_add(packet.len())
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "offset overflow"))?;
    let slot = out[0].as_mut();
    if slot.len() < required_len {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "output buffer is too small for packet",
        ));
    }

    slot[offset..required_len].copy_from_slice(packet);
    sizes[0] = packet.len();
    Ok(1)
}

fn split_gso_packet<B: AsMut<[u8]>>(
    input: &mut [u8],
    info: &OffloadInfo,
    out: &mut [B],
    sizes: &mut [usize],
    offset: usize,
) -> io::Result<usize> {
    let ip_version = input
        .first()
        .map(|first| first >> 4)
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "packet is empty"))?;
    let is_v6 = match ip_version {
        4 => false,
        6 => true,
        _ => {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "invalid ip header version",
            ));
        }
    };

    let protocol = match (info.gso_type, is_v6) {
        (GsoType::TcpV4, false) | (GsoType::TcpV6, true) => libc::IPPROTO_TCP as u8,
        (GsoType::UdpL4, _) => libc::IPPROTO_UDP as u8,
        (GsoType::TcpV4, true) | (GsoType::TcpV6, false) => {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "gso type does not match packet ip version",
            ));
        }
        _ => {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "unsupported gso type",
            ));
        }
    };

    let hdr_len = validated_hdr_len(input, info, protocol)?;
    let payload_len = input
        .len()
        .checked_sub(hdr_len)
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "packet is too short"))?;
    let gso_size = usize::from(info.gso_size);
    let segment_count = if payload_len == 0 {
        0
    } else {
        payload_len.div_ceil(gso_size)
    };
    ensure_output_slots(out.len(), sizes.len(), segment_count)?;

    let (src_addr_offset, addr_len) = if is_v6 {
        (IPV6_SRC_ADDR_OFFSET, 16)
    } else {
        input[10] = 0;
        input[11] = 0;
        (IPV4_SRC_ADDR_OFFSET, 4)
    };

    let transport_csum_at = usize::from(info.csum_start) + usize::from(info.csum_offset);
    input[transport_csum_at] = 0;
    input[transport_csum_at + 1] = 0;

    let src_addr_bytes = input[src_addr_offset..src_addr_offset + addr_len].to_vec();
    let dst_addr_bytes = input[src_addr_offset + addr_len..src_addr_offset + 2 * addr_len].to_vec();
    let transport_header_len = hdr_len - usize::from(info.csum_start);
    let first_tcp_seq_num = if protocol == libc::IPPROTO_TCP as u8 {
        read_u32_be(input, usize::from(info.csum_start) + 4)
    } else {
        0
    };

    for index in 0..segment_count {
        let data_offset = hdr_len + index * gso_size;
        let segment_data_len = (payload_len - index * gso_size).min(gso_size);
        let total_len = hdr_len + segment_data_len;
        let required_len = offset
            .checked_add(total_len)
            .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "offset overflow"))?;
        let slot = out[index].as_mut();
        if slot.len() < required_len {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "output buffer is too small for gso segment",
            ));
        }
        let segment = &mut slot[offset..required_len];

        segment[..usize::from(info.csum_start)]
            .copy_from_slice(&input[..usize::from(info.csum_start)]);
        segment[usize::from(info.csum_start)..hdr_len]
            .copy_from_slice(&input[usize::from(info.csum_start)..hdr_len]);
        segment[hdr_len..total_len]
            .copy_from_slice(&input[data_offset..data_offset + segment_data_len]);

        if is_v6 {
            let payload_with_transport = total_len - 40;
            write_u16_be(segment, 4, payload_with_transport as u16);
        } else {
            if index > 0 {
                let id = read_u16_be(segment, 4).wrapping_add(index as u16);
                write_u16_be(segment, 4, id);
            }
            write_u16_be(segment, 2, total_len as u16);
            let ipv4_checksum = !checksum(&segment[..usize::from(info.csum_start)], 0);
            write_u16_be(segment, 10, ipv4_checksum);
        }

        if protocol == libc::IPPROTO_TCP as u8 {
            let tcp_seq = first_tcp_seq_num.wrapping_add(info.gso_size as u32 * index as u32);
            write_u32_be(segment, usize::from(info.csum_start) + 4, tcp_seq);
            if index + 1 != segment_count {
                segment[usize::from(info.csum_start) + TCP_FLAGS_OFFSET] &=
                    !(TCP_FLAG_FIN | TCP_FLAG_PSH);
            }
        } else {
            let udp_len = (transport_header_len + segment_data_len) as u16;
            write_u16_be(segment, usize::from(info.csum_start) + 4, udp_len);
        }

        let pseudo_len = (transport_header_len + segment_data_len) as u16;
        let transport_csum = !checksum(
            &segment[usize::from(info.csum_start)..total_len],
            pseudo_header_checksum_no_fold(protocol, &src_addr_bytes, &dst_addr_bytes, pseudo_len),
        );
        write_u16_be(segment, transport_csum_at, transport_csum);
        sizes[index] = total_len;
    }

    Ok(segment_count)
}

fn validated_hdr_len(input: &[u8], info: &OffloadInfo, protocol: u8) -> io::Result<usize> {
    let csum_start = usize::from(info.csum_start);
    if protocol == libc::IPPROTO_UDP as u8 {
        let hdr_len = csum_start + UDP_HEADER_LEN;
        if input.len() < hdr_len {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "packet is shorter than udp header",
            ));
        }
        validate_checksum_bounds(input.len(), info.csum_start, info.csum_offset)?;
        return Ok(hdr_len);
    }

    if input.len() <= csum_start + 12 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "packet is too short for tcp gso metadata",
        ));
    }

    let tcp_hdr_len = usize::from(input[csum_start + 12] >> 4) * 4;
    if !(20..=60).contains(&tcp_hdr_len) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "tcp header len is invalid",
        ));
    }

    let hdr_len = csum_start + tcp_hdr_len;
    if input.len() < hdr_len {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "packet is shorter than tcp header",
        ));
    }

    validate_checksum_bounds(input.len(), info.csum_start, info.csum_offset)?;
    Ok(hdr_len)
}

fn validate_checksum_bounds(len: usize, csum_start: u16, csum_offset: u16) -> io::Result<()> {
    let checksum_at = usize::from(csum_start) + usize::from(csum_offset);
    if checksum_at + 1 >= len {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "checksum offset exceeds packet length",
        ));
    }
    Ok(())
}

fn ensure_output_slots(out_len: usize, sizes_len: usize, required: usize) -> io::Result<()> {
    if out_len < required || sizes_len < required {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "not enough output slots for split result",
        ));
    }
    Ok(())
}

fn gso_none_checksum(packet: &mut [u8], csum_start: u16, csum_offset: u16) {
    let checksum_at = usize::from(csum_start) + usize::from(csum_offset);
    let initial = u64::from(read_u16_be(packet, checksum_at));
    packet[checksum_at] = 0;
    packet[checksum_at + 1] = 0;
    let transport_checksum = !checksum(&packet[usize::from(csum_start)..], initial);
    write_u16_be(packet, checksum_at, transport_checksum);
}

fn pseudo_header_checksum_no_fold(
    protocol: u8,
    src_addr: &[u8],
    dst_addr: &[u8],
    total_len: u16,
) -> u64 {
    let sum = checksum_no_fold(src_addr, 0);
    let sum = checksum_no_fold(dst_addr, sum);
    let len_bytes = total_len.to_be_bytes();
    checksum_no_fold(&[0, protocol, len_bytes[0], len_bytes[1]], sum)
}

fn checksum(bytes: &[u8], initial: u64) -> u16 {
    let mut accumulator = checksum_no_fold(bytes, initial);
    while accumulator > 0xffff {
        accumulator = (accumulator & 0xffff) + (accumulator >> 16);
    }
    accumulator as u16
}

fn checksum_no_fold(mut bytes: &[u8], mut accumulator: u64) -> u64 {
    while bytes.len() >= 4 {
        accumulator += u64::from(read_u32_be(bytes, 0));
        bytes = &bytes[4..];
    }

    if bytes.len() >= 2 {
        accumulator += u64::from(read_u16_be(bytes, 0));
        bytes = &bytes[2..];
    }

    if let Some(byte) = bytes.first() {
        accumulator += u64::from(*byte) << 8;
    }

    accumulator
}

fn read_u16_be(bytes: &[u8], offset: usize) -> u16 {
    u16::from_be_bytes([bytes[offset], bytes[offset + 1]])
}

fn write_u16_be(bytes: &mut [u8], offset: usize, value: u16) {
    bytes[offset..offset + 2].copy_from_slice(&value.to_be_bytes());
}

fn read_u32_be(bytes: &[u8], offset: usize) -> u32 {
    u32::from_be_bytes([
        bytes[offset],
        bytes[offset + 1],
        bytes[offset + 2],
        bytes[offset + 3],
    ])
}

fn write_u32_be(bytes: &mut [u8], offset: usize, value: u32) {
    bytes[offset..offset + 4].copy_from_slice(&value.to_be_bytes());
}

#[cfg(test)]
mod tests {
    use super::{
        GsoType, OffloadInfo, Packet, PacketRecycle, RecycleCounter, UDP_HEADER_LEN,
        VIRTIO_NET_HDR_F_NEEDS_CSUM, VIRTIO_NET_HDR_LEN,
    };
    use std::sync::{
        atomic::{AtomicUsize, Ordering},
        Arc,
    };

    struct TestRingRecycle {
        count: Arc<AtomicUsize>,
        _bytes: Box<[u8]>,
    }

    impl PacketRecycle for TestRingRecycle {
        fn recycle(&self) {
            self.count.fetch_add(1, Ordering::SeqCst);
        }
    }

    #[test]
    fn packet_drop_recycles_once() {
        let counter = RecycleCounter::new();
        let packet = Packet::from_recyclable(vec![1, 2, 3], counter.token(), None);

        drop(packet);

        assert_eq!(counter.load(), 1);
    }

    #[test]
    fn detach_recycles_once_and_drop_does_not_repeat() {
        let counter = RecycleCounter::new();
        let mut packet = Packet::from_recyclable(vec![1, 2, 3], counter.token(), None);

        packet.detach();
        assert!(packet.is_detached());
        assert_eq!(counter.load(), 1);

        drop(packet);

        assert_eq!(counter.load(), 1);
    }

    #[test]
    fn owned_packet_is_already_detached() {
        let packet = Packet::from_owned(vec![1, 2, 3]);

        assert!(packet.is_detached());
        assert_eq!(packet.as_bytes(), &[1, 2, 3]);
        assert_eq!(packet.len(), 3);
    }

    #[test]
    fn packet_reports_gso_metadata() {
        let packet = Packet::from_recyclable(
            vec![0; 128],
            RecycleCounter::new().token(),
            Some(OffloadInfo {
                gso_type: GsoType::TcpV4,
                gso_size: 1460,
                hdr_len: 40,
                csum_start: 20,
                csum_offset: 16,
                needs_csum: true,
            }),
        );

        assert!(packet.is_gso());
        assert!(packet.offload_info().is_some());
    }

    #[test]
    fn ring_backed_packet_drop_recycles_once() {
        let count = Arc::new(AtomicUsize::new(0));
        let mut bytes = vec![7, 8, 9].into_boxed_slice();
        let packet = Packet::from_ring(
            bytes.as_mut_ptr() as usize,
            bytes.len(),
            Arc::new(TestRingRecycle {
                count: Arc::clone(&count),
                _bytes: bytes,
            }),
            None,
        );

        assert!(!packet.is_detached());
        assert_eq!(packet.as_bytes(), &[7, 8, 9]);
        drop(packet);

        assert_eq!(count.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn ring_backed_packet_detach_copies_before_recycling() {
        let count = Arc::new(AtomicUsize::new(0));
        let mut bytes = vec![4, 5, 6].into_boxed_slice();
        let mut packet = Packet::from_ring(
            bytes.as_mut_ptr() as usize,
            bytes.len(),
            Arc::new(TestRingRecycle {
                count: Arc::clone(&count),
                _bytes: bytes,
            }),
            None,
        );

        packet.detach();

        assert!(packet.is_detached());
        assert_eq!(packet.as_bytes(), &[4, 5, 6]);
        assert_eq!(count.load(Ordering::SeqCst), 1);

        drop(packet);

        assert_eq!(count.load(Ordering::SeqCst), 1);
    }

    fn ipv4_checksum(header: &[u8]) -> u16 {
        let mut sum = 0u32;
        for chunk in header.chunks_exact(2) {
            sum += u32::from(u16::from_be_bytes([chunk[0], chunk[1]]));
        }

        while sum > 0xffff {
            sum = (sum & 0xffff) + (sum >> 16);
        }

        !(sum as u16)
    }

    fn build_ipv4_udp_packet(payload: &[u8]) -> Vec<u8> {
        let ip_header_len = 20usize;
        let udp_header_len = 8usize;
        let total_len = ip_header_len + udp_header_len + payload.len();
        let mut packet = vec![0u8; total_len];

        packet[0] = 0x45;
        packet[2..4].copy_from_slice(&(total_len as u16).to_be_bytes());
        packet[4..6].copy_from_slice(&0x1200u16.to_be_bytes());
        packet[8] = 64;
        packet[9] = libc::IPPROTO_UDP as u8;
        packet[12..16].copy_from_slice(&[10, 0, 0, 1]);
        packet[16..20].copy_from_slice(&[10, 0, 0, 2]);
        let header_checksum = ipv4_checksum(&packet[..ip_header_len]);
        packet[10..12].copy_from_slice(&header_checksum.to_be_bytes());

        packet[ip_header_len..ip_header_len + 2].copy_from_slice(&1000u16.to_be_bytes());
        packet[ip_header_len + 2..ip_header_len + 4].copy_from_slice(&2000u16.to_be_bytes());
        packet[ip_header_len + 4..ip_header_len + 6]
            .copy_from_slice(&((udp_header_len + payload.len()) as u16).to_be_bytes());
        packet[ip_header_len + udp_header_len..].copy_from_slice(payload);
        packet
    }

    fn prepend_virtio_net_hdr(
        packet: Vec<u8>,
        flags: u8,
        gso_type: u8,
        hdr_len: u16,
        gso_size: u16,
        csum_start: u16,
        csum_offset: u16,
    ) -> Vec<u8> {
        let mut raw = vec![0u8; VIRTIO_NET_HDR_LEN + packet.len()];
        raw[0] = flags;
        raw[1] = gso_type;
        raw[2..4].copy_from_slice(&hdr_len.to_ne_bytes());
        raw[4..6].copy_from_slice(&gso_size.to_ne_bytes());
        raw[6..8].copy_from_slice(&csum_start.to_ne_bytes());
        raw[8..10].copy_from_slice(&csum_offset.to_ne_bytes());
        raw[VIRTIO_NET_HDR_LEN..].copy_from_slice(&packet);
        raw
    }

    fn build_ipv4_udp_gso_packet(segment_payloads: &[&[u8]]) -> Vec<u8> {
        let payload = segment_payloads.concat();
        let mut packet = build_ipv4_udp_packet(&payload);
        packet[24..26].copy_from_slice(&(payload.len() as u16 + 8).to_be_bytes());

        prepend_virtio_net_hdr(
            packet,
            VIRTIO_NET_HDR_F_NEEDS_CSUM,
            5,
            0,
            segment_payloads[0].len() as u16,
            20,
            6,
        )
    }

    fn build_ipv4_udp_partial_checksum_packet(payload: &[u8]) -> Vec<u8> {
        let mut packet = build_ipv4_udp_packet(payload);
        let pseudo = super::pseudo_header_checksum_no_fold(
            libc::IPPROTO_UDP as u8,
            &packet[12..16],
            &packet[16..20],
            (8 + payload.len()) as u16,
        );
        let pseudo = super::checksum(&[], pseudo);
        packet[26..28].copy_from_slice(&pseudo.to_be_bytes());

        prepend_virtio_net_hdr(packet, VIRTIO_NET_HDR_F_NEEDS_CSUM, 0, 28, 0, 20, 6)
    }

    #[test]
    fn packet_lazily_parses_offload_info_from_virtio_header() {
        let payload = build_ipv4_udp_packet(b"lazy offload");
        let raw_packet = prepend_virtio_net_hdr(payload.clone(), 0, 0, 28, 0, 20, 6);
        let packet =
            Packet::from_recyclable_with_virtio_net_hdr(raw_packet, RecycleCounter::new().token());

        assert_eq!(&packet.as_bytes()[VIRTIO_NET_HDR_LEN..], payload.as_slice());
        assert_eq!(packet.len(), payload.len() + VIRTIO_NET_HDR_LEN);
        assert!(!packet.is_gso());

        let info = packet.offload_info().unwrap();
        assert_eq!(info.gso_type, GsoType::None);
        assert_eq!(info.hdr_len, 28);
        assert_eq!(info.csum_start, 20);
        assert_eq!(info.csum_offset, 6);
        assert!(!info.needs_csum);
    }

    #[test]
    fn split_into_copies_plain_packet_and_finishes_checksum() {
        let raw_packet = build_ipv4_udp_partial_checksum_packet(b"plain packet");
        let original_packet_bytes = raw_packet[VIRTIO_NET_HDR_LEN..].to_vec();
        let packet_bytes = build_ipv4_udp_packet(b"plain packet");
        let packet =
            Packet::from_recyclable_with_virtio_net_hdr(raw_packet, RecycleCounter::new().token());
        let mut out = [vec![0u8; 128]];
        let mut sizes = [0usize];

        let segments = packet.split_into(&mut out, &mut sizes, 4).unwrap();

        assert_eq!(segments, 1);
        assert_eq!(sizes[0], packet_bytes.len());
        assert_eq!(&out[0][4..24], &packet_bytes[..20]);
        assert_eq!(
            &out[0][4 + 20 + UDP_HEADER_LEN..4 + packet_bytes.len()],
            &packet_bytes[20 + UDP_HEADER_LEN..]
        );
        assert_ne!(u16::from_be_bytes([out[0][30], out[0][31]]), 0);
        assert_eq!(
            &packet.as_bytes()[VIRTIO_NET_HDR_LEN..],
            original_packet_bytes.as_slice()
        );
    }

    #[test]
    fn split_into_splits_ipv4_udp_packet() {
        let payloads = [b"ABCD".as_slice(), b"EFG".as_slice()];
        let packet_bytes = build_ipv4_udp_packet(&payloads.concat());
        let packet = Packet::from_recyclable_with_virtio_net_hdr(
            build_ipv4_udp_gso_packet(&payloads),
            RecycleCounter::new().token(),
        );
        let mut out = [vec![0u8; 128], vec![0u8; 128]];
        let mut sizes = [0usize; 2];

        let segments = packet.split_into(&mut out, &mut sizes, 2).unwrap();

        assert_eq!(segments, 2);
        assert_eq!(sizes, [32, 31]);
        assert_eq!(u16::from_be_bytes([out[0][4], out[0][5]]), 32);
        assert_eq!(u16::from_be_bytes([out[1][4], out[1][5]]), 31);
        assert_eq!(u16::from_be_bytes([out[0][26], out[0][27]]), 12);
        assert_eq!(u16::from_be_bytes([out[1][26], out[1][27]]), 11);
        assert_eq!(&out[0][30..34], payloads[0]);
        assert_eq!(&out[1][30..33], payloads[1]);
        assert_eq!(
            &packet.as_bytes()[VIRTIO_NET_HDR_LEN..],
            packet_bytes.as_slice()
        );
    }

    #[test]
    fn split_into_matches_before_and_after_detach() {
        let payloads = [b"segment-1".as_slice(), b"tail".as_slice()];
        let packet_bytes = build_ipv4_udp_packet(&payloads.concat());
        let count = Arc::new(AtomicUsize::new(0));
        let mut bytes = build_ipv4_udp_gso_packet(&payloads).into_boxed_slice();
        let mut packet = Packet::from_ring_with_virtio_net_hdr(
            bytes.as_mut_ptr() as usize,
            bytes.len(),
            Arc::new(TestRingRecycle {
                count: Arc::clone(&count),
                _bytes: bytes,
            }),
        );
        let mut before_out = [vec![0u8; 128], vec![0u8; 128]];
        let mut after_out = [vec![0u8; 128], vec![0u8; 128]];
        let mut before_sizes = [0usize; 2];
        let mut after_sizes = [0usize; 2];

        let before = packet
            .split_into(&mut before_out, &mut before_sizes, 0)
            .unwrap();
        packet.detach();
        let after = packet
            .split_into(&mut after_out, &mut after_sizes, 0)
            .unwrap();

        assert_eq!(before, after);
        assert_eq!(before_sizes, after_sizes);
        assert_eq!(
            before_out[0][..before_sizes[0]],
            after_out[0][..after_sizes[0]]
        );
        assert_eq!(
            before_out[1][..before_sizes[1]],
            after_out[1][..after_sizes[1]]
        );
        assert_eq!(
            &packet.as_bytes()[VIRTIO_NET_HDR_LEN..],
            packet_bytes.as_slice()
        );
        assert_eq!(count.load(Ordering::SeqCst), 1);
    }
}
