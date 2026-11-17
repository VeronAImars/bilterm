use super::frame::*;
use super::limits::*;

/// Deterministic xorshift64* so fuzz-style tests are reproducible (no extra dev-dependency).
pub(crate) struct Rng(pub u64);
impl Rng {
    pub fn next(&mut self) -> u64 {
        self.0 ^= self.0 >> 12;
        self.0 ^= self.0 << 25;
        self.0 ^= self.0 >> 27;
        self.0.wrapping_mul(0x2545F4914F6CDD1D)
    }
    pub fn below(&mut self, n: usize) -> usize {
        (self.next() % n.max(1) as u64) as usize
    }
    pub fn bytes(&mut self, n: usize) -> Vec<u8> {
        (0..n).map(|_| self.next() as u8).collect()
    }
}

const C2H: Direction = Direction::ClientToHost;

fn sample(payload: &[u8]) -> Vec<u8> {
    encode_frame(MsgType::Ping as u8, 0, payload, C2H).unwrap()
}

#[test]
fn round_trip() {
    let bytes = sample(br#"{"t":"ping","n":1}"#);
    let (f, used) = decode_one(&bytes, MAX_CLIENT_FRAME_BYTES).unwrap().unwrap();
    assert_eq!(used, bytes.len());
    assert_eq!(f.proto_version, ATTACH_PROTO_VERSION);
    assert_eq!(f.msg_type(C2H), Some(MsgType::Ping));
    assert_eq!(f.msg_type(Direction::HostToClient), None);
    assert_eq!(f.payload, br#"{"t":"ping","n":1}"#);
}

#[test]
fn truncation_at_every_byte_boundary_needs_more() {
    let bytes = sample(b"hello world");
    for cut in 0..bytes.len() {
        assert_eq!(decode_one(&bytes[..cut], MAX_CLIENT_FRAME_BYTES), Ok(None), "cut {cut}");
    }
    assert!(decode_one(&bytes, MAX_CLIENT_FRAME_BYTES).unwrap().is_some());
}

#[test]
fn trailing_bytes_are_left_for_next_frame() {
    let mut bytes = sample(b"a");
    let first = bytes.len();
    bytes.extend(sample(b"bb"));
    let (f, used) = decode_one(&bytes, MAX_CLIENT_FRAME_BYTES).unwrap().unwrap();
    assert_eq!((f.payload.as_slice(), used), (&b"a"[..], first));
}

#[test]
fn bad_magic_reported_early() {
    assert_eq!(decode_one(b"X", 100), Err(FrameError::BadMagic));
    assert_eq!(decode_one(b"BTHX", 100), Err(FrameError::BadMagic));
    assert_eq!(decode_one(b"", 100), Ok(None));
    assert_eq!(decode_one(b"BT", 100), Ok(None));
}

#[test]
fn version_zero_and_newer_rejected() {
    for v in [0u16, ATTACH_PROTO_VERSION + 1, u16::MAX] {
        let mut b = sample(b"x");
        b[4..6].copy_from_slice(&v.to_be_bytes());
        assert_eq!(decode_one(&b, 100), Err(FrameError::UnsupportedVersion(v)));
    }
}

#[test]
fn reserved_flags_rejected() {
    for bit in 1..8u8 {
        let mut b = sample(b"x");
        b[7] = 1 << bit;
        assert_eq!(decode_one(&b, 100), Err(FrameError::BadFlags(1 << bit)), "bit {bit}");
    }
    let mut b = sample(b"x");
    b[7] = FLAG_BINARY;
    assert!(decode_one(&b, 100).unwrap().is_some());
    assert_eq!(encode_frame(1, FLAG_COMPRESSED, b"", C2H), Err(FrameError::BadFlags(FLAG_COMPRESSED)));
}

#[test]
fn oversize_rejected_from_header_alone_without_payload() {
    // Header declares u32::MAX; no payload bytes supplied. Must error, not wait or allocate.
    let mut h = Vec::new();
    h.extend_from_slice(&MAGIC);
    h.extend_from_slice(&ATTACH_PROTO_VERSION.to_be_bytes());
    h.extend_from_slice(&[MsgType::Input as u8, 0]);
    h.extend_from_slice(&u32::MAX.to_be_bytes());
    assert_eq!(decode_one(&h, MAX_CLIENT_FRAME_BYTES), Err(FrameError::TooLarge { len: u32::MAX as usize, max: MAX_CLIENT_FRAME_BYTES }));
    let mut r = FrameReader::new(C2H);
    assert!(matches!(r.feed(&h), Err(FrameError::TooLarge { .. })));
    assert!(r.buffered() == 0);
    assert_eq!(r.feed(b"more"), Err(FrameError::Poisoned));
}

#[test]
fn exactly_max_accepted_and_max_plus_one_rejected() {
    for (dir, max) in [(C2H, MAX_CLIENT_FRAME_BYTES), (Direction::HostToClient, MAX_ATTACH_FRAME_BYTES)] {
        assert_eq!(dir.max_payload(), max);
        let ok = encode_frame(1, 0, &vec![b'x'; max], dir).unwrap();
        assert_eq!(ok.len(), HEADER_LEN + max);
        let (f, _) = decode_one(&ok, max).unwrap().unwrap();
        assert_eq!(f.payload.len(), max);
        assert_eq!(encode_frame(1, 0, &vec![b'x'; max + 1], dir), Err(FrameError::TooLarge { len: max + 1, max }));
        let mut over = ok.clone();
        over[8..12].copy_from_slice(&((max + 1) as u32).to_be_bytes());
        assert!(matches!(decode_one(&over, max), Err(FrameError::TooLarge { .. })));
    }
}

#[test]
fn handshake_cap_is_4k_total() {
    assert_eq!(HEADER_LEN + HANDSHAKE_MAX_PAYLOAD, HANDSHAKE_MAX_BYTES);
    assert!(encode_frame_with_max(1, 0, &vec![0; HANDSHAKE_MAX_PAYLOAD], HANDSHAKE_MAX_PAYLOAD).is_ok());
    assert!(encode_frame_with_max(1, 0, &vec![0; HANDSHAKE_MAX_PAYLOAD + 1], HANDSHAKE_MAX_PAYLOAD).is_err());
}

#[test]
fn unknown_type_still_decodes_so_it_can_be_skipped() {
    let b = encode_frame(0x77, 0, b"{}", C2H).unwrap();
    let (f, used) = decode_one(&b, MAX_CLIENT_FRAME_BYTES).unwrap().unwrap();
    assert_eq!(used, b.len());
    assert_eq!(f.msg_type(C2H), None);
    assert_eq!(MsgType::from_u8(0x77), None);
}

#[test]
fn msg_type_table_is_unique_and_directional() {
    let mut seen = std::collections::HashSet::new();
    for t in MsgType::ALL {
        assert!(seen.insert(t as u8));
        assert_eq!(MsgType::from_u8(t as u8), Some(t));
        assert_eq!(t.direction() == C2H, (t as u8) < 0x80);
    }
    assert_eq!(seen.len(), 23);
}

#[test]
fn reader_reassembles_at_every_split_point() {
    let mut stream = sample(b"first");
    stream.extend(sample(b""));
    stream.extend(sample(b"third frame"));
    for split in 0..=stream.len() {
        let mut r = FrameReader::new(C2H);
        let mut frames = r.push_all(&stream[..split]).unwrap();
        frames.extend(r.push_all(&stream[split..]).unwrap());
        let got: Vec<_> = frames.iter().map(|f| f.payload.clone()).collect();
        assert_eq!(got, vec![b"first".to_vec(), b"".to_vec(), b"third frame".to_vec()], "split {split}");
        assert_eq!(r.buffered(), 0);
    }
}

#[test]
fn reader_byte_at_a_time() {
    let stream: Vec<u8> = [sample(b"ab"), sample(b"c")].concat();
    let mut r = FrameReader::new(C2H);
    let mut n = 0;
    for b in &stream {
        n += r.push_all(std::slice::from_ref(b)).unwrap().len();
    }
    assert_eq!(n, 2);
}

#[test]
fn reader_memory_is_bounded_by_one_frame() {
    // A peer streaming endless valid max-size frames never makes the reader hold more than one.
    let frame = encode_frame(1, 0, &vec![0u8; 1000], C2H).unwrap();
    let mut r = FrameReader::with_max_payload(1000);
    for _ in 0..200 {
        let frames = r.push_all(&frame).unwrap();
        assert_eq!(frames.len(), 1);
        assert!(r.buffered() <= HEADER_LEN + 1000);
    }
    // Garbage that declares a huge frame is rejected before buffering anything big.
    let mut r = FrameReader::new(C2H);
    let mut bad = sample(b"x");
    bad[8..12].copy_from_slice(&u32::MAX.to_be_bytes());
    assert!(r.push_all(&bad).is_err());
    assert!(r.buffered() == 0);
}

#[test]
fn fuzz_random_bytes_never_panic() {
    let mut rng = Rng(0x9E3779B97F4A7C15);
    for _ in 0..20_000 {
        let n = rng.below(64);
        let buf = rng.bytes(n);
        let _ = decode_one(&buf, MAX_CLIENT_FRAME_BYTES);
        let mut r = FrameReader::new(Direction::HostToClient);
        let _ = r.push_all(&buf);
        assert!(r.buffered() <= HEADER_LEN + MAX_ATTACH_FRAME_BYTES);
    }
}

#[test]
fn fuzz_mutated_valid_frames_never_panic_and_stay_bounded() {
    let mut rng = Rng(42);
    for _ in 0..20_000 {
        let plen = rng.below(48);
        let payload = rng.bytes(plen);
        let mut b = encode_frame(rng.below(256) as u8, 0, &payload, C2H).unwrap();
        for _ in 0..1 + rng.below(3) {
            let i = rng.below(b.len());
            b[i] = rng.next() as u8;
        }
        if let Ok(Some((f, used))) = decode_one(&b, MAX_CLIENT_FRAME_BYTES) {
            assert!(used <= b.len());
            assert!(f.payload.len() <= MAX_CLIENT_FRAME_BYTES);
        }
        let mut r = FrameReader::new(C2H);
        let cut = rng.below(b.len() + 1);
        let _ = r.push_all(&b[..cut]);
        let _ = r.push_all(&b[cut..]);
    }
}

#[test]
fn fuzz_encode_decode_round_trip() {
    let mut rng = Rng(7);
    for _ in 0..2_000 {
        let n = rng.below(2000);
        let payload = rng.bytes(n);
        let code = rng.next() as u8;
        let flags = (rng.next() as u8) & KNOWN_FLAGS;
        let b = encode_frame(code, flags, &payload, C2H).unwrap();
        let (f, used) = decode_one(&b, MAX_CLIENT_FRAME_BYTES).unwrap().unwrap();
        assert_eq!((f.type_code, f.flags, f.payload, used), (code, flags, payload, b.len()));
    }
}
