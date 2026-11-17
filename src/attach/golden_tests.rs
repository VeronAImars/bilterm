//! Stored v1 frames (hex). Changing any of these bytes is a wire-compatibility break: bump
//! `ATTACH_PROTO_VERSION` and add new vectors instead of editing these.

use super::frame::*;
use super::limits::*;
use super::msg::*;

fn unhex(s: &str) -> Vec<u8> {
    (0..s.len()).step_by(2).map(|i| u8::from_str_radix(&s[i..i + 2], 16).unwrap()).collect()
}

const HELLO: &str = "4254485300010100000000657b2274223a2268656c6c6f222c2270726f746f5f6d696e223a312c2270726f746f5f6d6178223a312c22636c69656e745f6b696e64223a22747569222c22636c69656e745f6e616d65223a2262696c7465726d222c2263617073223a5b2264696666225d7d";
const ATTACH: &str = "4254485300010200000000527b2274223a22617474616368222c2273657373696f6e223a226d61696e222c22726f6c65223a227277222c22636f6c73223a38302c22726f7773223a32342c2270616e655f66696c746572223a6e756c6c7d";
const INPUT: &str = "4254485300010400000000287b2274223a22696e707574222c2270616e65223a312c22646174615f623634223a2262484d4b227d";
const PING: &str = "4254485300010800000000127b2274223a2270696e67222c226e223a377d";
const HELLO_OK: &str = "42544853000181000000008e7b2274223a2268656c6c6f5f6f6b222c2270726f746f223a312c22686f73745f76657273696f6e223a22302e312e30222c22686f73745f6e616d65223a2268222c22686f73745f706964223a34322c2263617073223a5b5d2c2273657373696f6e73223a5b7b226e616d65223a226d61696e222c2270616e6573223a327d5d2c22636c69656e745f6964223a337d";
const ERROR: &str = "4254485300018c000000003a7b2274223a226572726f72222c22636f6465223a226672616d655f746f6f5f6c61726765222c226d657373616765223a22746f6f20626967227d";
const BYE: &str = "4254485300018e00000000247b2274223a22627965222c22726561736f6e223a22686f73745f73687574646f776e227d";

fn client_cases() -> Vec<(&'static str, ClientMsg)> {
    vec![
        (HELLO, ClientMsg::Hello { proto_min: 1, proto_max: 1, client_kind: ClientKind::Tui, client_name: "bilterm".into(), caps: vec!["diff".into()] }),
        (ATTACH, ClientMsg::Attach { session: "main".into(), role: Role::Rw, cols: 80, rows: 24, pane_filter: None }),
        (INPUT, ClientMsg::Input { pane: 1, data_b64: "bHMK".into() }),
        (PING, ClientMsg::Ping { n: 7 }),
    ]
}

fn host_cases() -> Vec<(&'static str, HostMsg)> {
    vec![
        (HELLO_OK, HostMsg::HelloOk { proto: 1, host_version: "0.1.0".into(), host_name: "h".into(), host_pid: 42, caps: vec![], sessions: vec![SessionInfo { name: "main".into(), panes: 2 }], client_id: 3 }),
        (ERROR, HostMsg::Error { code: ErrorCode::FrameTooLarge, message: "too big".into() }),
        (BYE, HostMsg::Bye { reason: ByeReason::HostShutdown }),
    ]
}

#[test]
fn client_vectors_encode_and_decode_exactly() {
    for (hex, msg) in client_cases() {
        let bytes = unhex(hex);
        assert_eq!(msg.encode().unwrap(), bytes, "encode {msg:?}");
        let (f, used) = decode_one(&bytes, MAX_CLIENT_FRAME_BYTES).unwrap().unwrap();
        assert_eq!(used, bytes.len());
        assert_eq!(ClientMsg::decode(&f).unwrap(), msg);
    }
}

#[test]
fn host_vectors_encode_and_decode_exactly() {
    for (hex, msg) in host_cases() {
        let bytes = unhex(hex);
        assert_eq!(msg.encode().unwrap(), bytes, "encode {msg:?}");
        let (f, used) = decode_one(&bytes, MAX_ATTACH_FRAME_BYTES).unwrap().unwrap();
        assert_eq!(used, bytes.len());
        assert_eq!(HostMsg::decode(&f).unwrap(), msg);
    }
}

#[test]
fn header_layout_is_pinned() {
    // magic | version=1 | type=0x08 (Ping) | flags=0 | len=0x12
    assert_eq!(&unhex(PING)[..HEADER_LEN], &[0x42, 0x54, 0x48, 0x53, 0x00, 0x01, 0x08, 0x00, 0x00, 0x00, 0x00, 0x12]);
}

#[test]
fn msg_type_codes_are_pinned() {
    let pins: [(MsgType, u8); 23] = [
        (MsgType::Hello, 0x01), (MsgType::Attach, 0x02), (MsgType::Detach, 0x03), (MsgType::Input, 0x04),
        (MsgType::Act, 0x05), (MsgType::Resize, 0x06), (MsgType::Ack, 0x07), (MsgType::Ping, 0x08),
        (MsgType::ClipboardReply, 0x09), (MsgType::HelloOk, 0x81), (MsgType::Refuse, 0x82),
        (MsgType::Layout, 0x83), (MsgType::Snapshot, 0x84), (MsgType::Delta, 0x85), (MsgType::Scrollback, 0x86),
        (MsgType::Bell, 0x87), (MsgType::Title, 0x88), (MsgType::Exit, 0x89), (MsgType::ClipboardSet, 0x8a),
        (MsgType::Resized, 0x8b), (MsgType::Error, 0x8c), (MsgType::Pong, 0x8d), (MsgType::Bye, 0x8e),
    ];
    for (t, c) in pins {
        assert_eq!(t as u8, c);
    }
}

#[test]
fn golden_rejections() {
    // Newer proto version, reserved flag, oversize client frame: all rejected from header bytes.
    let mut v2 = unhex(PING);
    v2[5] = 2;
    assert_eq!(decode_one(&v2, MAX_CLIENT_FRAME_BYTES), Err(FrameError::UnsupportedVersion(2)));
    let mut fl = unhex(PING);
    fl[7] = 0x02;
    assert_eq!(decode_one(&fl, MAX_CLIENT_FRAME_BYTES), Err(FrameError::BadFlags(2)));
    let mut big = unhex(PING);
    big[8..12].copy_from_slice(&0x0001_0001u32.to_be_bytes());
    assert!(matches!(decode_one(&big, MAX_CLIENT_FRAME_BYTES), Err(FrameError::TooLarge { .. })));
}
