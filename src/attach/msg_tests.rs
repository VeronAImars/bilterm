use super::frame::*;
use super::frame_tests::Rng;
use super::limits::*;
use super::msg::*;

fn hello() -> ClientMsg {
    ClientMsg::Hello { proto_min: 1, proto_max: 1, client_kind: ClientKind::Tui, client_name: "t".into(), caps: vec!["diff".into()] }
}

fn rt_client(m: ClientMsg) {
    let b = m.encode().unwrap();
    let (f, _) = decode_one(&b, MAX_CLIENT_FRAME_BYTES).unwrap().unwrap();
    assert_eq!(ClientMsg::decode(&f).unwrap(), m);
}

fn rt_host(m: HostMsg) {
    let b = m.encode().unwrap();
    let (f, _) = decode_one(&b, MAX_ATTACH_FRAME_BYTES).unwrap().unwrap();
    assert_eq!(HostMsg::decode(&f).unwrap(), m);
}

fn frame(code: u8, json: &str) -> Frame {
    Frame { proto_version: 1, type_code: code, flags: 0, payload: json.as_bytes().to_vec() }
}

#[test]
fn every_client_message_round_trips() {
    rt_client(hello());
    rt_client(ClientMsg::Attach { session: "main".into(), role: Role::Rw, cols: 80, rows: 24, pane_filter: Some(vec![1, 2]) });
    rt_client(ClientMsg::Detach {});
    rt_client(ClientMsg::Input { pane: 3, data_b64: "aGk=".into() });
    rt_client(ClientMsg::Act { cmd: ":split h".into() });
    rt_client(ClientMsg::Resize { cols: 100, rows: 30, pixel_w: Some(800), pixel_h: None });
    rt_client(ClientMsg::Ack { pane: 1, seq: u64::MAX });
    rt_client(ClientMsg::Ping { n: 9 });
    rt_client(ClientMsg::ClipboardReply { req_id: 4, data_b64: None, denied: true });
}

#[test]
fn every_host_message_round_trips() {
    let mut body = serde_json::Map::new();
    body.insert("cursor".into(), serde_json::json!({"x":1,"y":2}));
    rt_host(HostMsg::HelloOk { proto: 1, host_version: "0.1".into(), host_name: "h".into(), host_pid: 7, caps: vec![], sessions: vec![SessionInfo { name: "main".into(), panes: 2 }], client_id: 5 });
    rt_host(HostMsg::Refuse { reason: "no".into(), host_proto_min: 1, host_proto_max: 1 });
    rt_host(HostMsg::Layout { body: body.clone() });
    rt_host(HostMsg::Snapshot { pane: 1, seq: 2, generation: 3, body: body.clone() });
    rt_host(HostMsg::Delta { pane: 1, base_gen: 3, generation: 4, seq: 5, body: body.clone() });
    rt_host(HostMsg::Scrollback { pane: 1, from: 0, body });
    rt_host(HostMsg::Bell { pane: 1 });
    rt_host(HostMsg::Title { pane: 1, title: "t".into() });
    rt_host(HostMsg::Exit { pane: 1, status: Some(-1) });
    rt_host(HostMsg::ClipboardSet { pane: 1, data_b64: "aA==".into(), kind: "c".into() });
    rt_host(HostMsg::Resized { cols: 80, rows: 24, owner_client: None });
    rt_host(HostMsg::Error { code: ErrorCode::ReadOnly, message: "ro".into() });
    rt_host(HostMsg::Pong { n: 1 });
    rt_host(HostMsg::Bye { reason: ByeReason::Kicked });
}

#[test]
fn unknown_fields_ignored_and_unknown_enums_tolerated() {
    let f = frame(MsgType::Ping as u8, r#"{"t":"ping","n":3,"future":[1,2,3]}"#);
    assert_eq!(ClientMsg::decode(&f), Ok(ClientMsg::Ping { n: 3 }));
    let f = frame(MsgType::Error as u8, r#"{"t":"error","code":"from_the_future","message":"m"}"#);
    assert_eq!(HostMsg::decode(&f), Ok(HostMsg::Error { code: ErrorCode::Unknown, message: "m".into() }));
}

#[test]
fn unknown_and_wrong_direction_types_report_unknown() {
    assert_eq!(ClientMsg::decode(&frame(0x7f, "{}")), Err(MsgError::UnknownType(0x7f)));
    assert_eq!(ClientMsg::decode(&frame(MsgType::Pong as u8, r#"{"t":"pong","n":1}"#)), Err(MsgError::UnknownType(MsgType::Pong as u8)));
    assert_eq!(MsgError::UnknownType(1).code(), ErrorCode::UnknownMessage);
}

#[test]
fn header_tag_mismatch_and_flags_rejected() {
    assert_eq!(ClientMsg::decode(&frame(MsgType::Ping as u8, r#"{"t":"detach"}"#)), Err(MsgError::TypeMismatch));
    let mut f = frame(MsgType::Ping as u8, r#"{"t":"ping","n":1}"#);
    f.flags = FLAG_BINARY;
    assert_eq!(ClientMsg::decode(&f), Err(MsgError::BinaryUnsupported));
}

#[test]
fn malformed_json_is_an_error_not_a_panic() {
    for p in ["", "{", "null", "[]", r#"{"t":"ping"}"#, r#"{"t":"ping","n":"x"}"#, r#"{"t":"ping","n":-1}"#, "\u{0}\u{1}"] {
        assert!(matches!(ClientMsg::decode(&frame(MsgType::Ping as u8, p)), Err(MsgError::Json(_))), "{p:?}");
    }
    let mut f = frame(MsgType::Ping as u8, "");
    f.payload = vec![0xff, 0xfe, 0x80];
    assert!(ClientMsg::decode(&f).is_err());
    let deep = format!("{}{}", "[".repeat(100_000), "]".repeat(100_000));
    assert!(HostMsg::decode(&frame(MsgType::Layout as u8, &deep)).is_err());
}

#[test]
fn attach_dims_bounded_before_use() {
    let mk = |c, r| ClientMsg::Attach { session: "s".into(), role: Role::Ro, cols: c, rows: r, pane_filter: None };
    assert!(mk(MAX_ATTACH_COLS, MAX_ATTACH_ROWS).validate().is_ok());
    assert!(mk(MAX_ATTACH_COLS + 1, 1).validate().is_err());
    assert!(mk(1, MAX_ATTACH_ROWS + 1).validate().is_err());
    assert!(mk(0, 1).validate().is_err());
    assert!(mk(u16::MAX, u16::MAX).validate().is_err());
    let r = |c, r, pw| ClientMsg::Resize { cols: c, rows: r, pixel_w: pw, pixel_h: None };
    assert!(r(80, 24, Some(MAX_ATTACH_PIXELS)).validate().is_ok());
    assert!(r(80, 24, Some(MAX_ATTACH_PIXELS + 1)).validate().is_err());
    assert!(r(501, 24, None).validate().is_err());
}

#[test]
fn string_and_count_caps_exact_and_over() {
    let hello_with = |name: usize, caps: usize, cap_len: usize| ClientMsg::Hello {
        proto_min: 1, proto_max: 1, client_kind: ClientKind::Ctl,
        client_name: "n".repeat(name), caps: vec!["c".repeat(cap_len); caps],
    };
    assert!(hello_with(MAX_HELLO_STRING_BYTES, MAX_HELLO_CAPS, MAX_HELLO_STRING_BYTES).validate().is_ok());
    assert!(hello_with(MAX_HELLO_STRING_BYTES + 1, 0, 1).validate().is_err());
    assert!(hello_with(1, MAX_HELLO_CAPS + 1, 1).validate().is_err());
    assert!(hello_with(1, 1, MAX_HELLO_STRING_BYTES + 1).validate().is_err());

    let inp = |n| ClientMsg::Input { pane: 0, data_b64: "A".repeat(n) };
    assert!(inp(MAX_INPUT_B64_LEN).validate().is_ok());
    assert!(inp(MAX_INPUT_B64_LEN + 1).validate().is_err());
    let act = |n| ClientMsg::Act { cmd: "x".repeat(n) };
    assert!(act(MAX_ACT_CMD_BYTES).validate().is_ok());
    assert!(act(MAX_ACT_CMD_BYTES + 1).validate().is_err());
    let pf = |n| ClientMsg::Attach { session: "s".into(), role: Role::Rw, cols: 1, rows: 1, pane_filter: Some(vec![0; n]) };
    assert!(pf(MAX_PANE_FILTER).validate().is_ok());
    assert!(pf(MAX_PANE_FILTER + 1).validate().is_err());
}

#[test]
fn oversize_messages_fail_to_encode() {
    let big = ClientMsg::Act { cmd: "x".repeat(MAX_CLIENT_FRAME_BYTES) };
    assert!(matches!(big.encode(), Err(MsgError::Frame(FrameError::TooLarge { .. }))));
    let mut body = serde_json::Map::new();
    body.insert("blob".into(), "x".repeat(MAX_ATTACH_FRAME_BYTES).into());
    let snap = HostMsg::Snapshot { pane: 0, seq: 0, generation: 0, body };
    assert_eq!(snap.encode().unwrap_err().code(), ErrorCode::FrameTooLarge);
}

#[test]
fn fuzz_random_payloads_never_panic() {
    let mut rng = Rng(0xDEADBEEF);
    let seeds: Vec<Vec<u8>> = vec![
        br#"{"t":"hello","proto_min":1,"proto_max":1,"client_kind":"tui","client_name":"x"}"#.to_vec(),
        br#"{"t":"attach","session":"s","role":"rw","cols":80,"rows":24}"#.to_vec(),
        br#"{"t":"input","pane":1,"data_b64":"aGk="}"#.to_vec(),
        br#"{"t":"resize","cols":1,"rows":1}"#.to_vec(),
    ];
    for i in 0..20_000 {
        let mut p = if i % 2 == 0 { let n = rng.below(80); rng.bytes(n) } else { seeds[rng.below(seeds.len())].clone() };
        if !p.is_empty() {
            for _ in 0..rng.below(4) {
                let k = rng.below(p.len());
                p[k] = rng.next() as u8;
            }
        }
        let code = MsgType::ALL[rng.below(MsgType::ALL.len())] as u8;
        let f = Frame { proto_version: 1, type_code: code, flags: 0, payload: p };
        let _ = ClientMsg::decode(&f);
        let _ = HostMsg::decode(&f);
    }
}
