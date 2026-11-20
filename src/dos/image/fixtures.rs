//! Test-only builders for tiny ZIP and ISO 9660 images, plus a synthetic "Live CD".

use std::io::Write;

use flate2::Crc;
use flate2::write::DeflateEncoder;

/// Builds a ZIP from `(name, data, deflate)` tuples.
pub fn make_zip(entries: &[(&str, &[u8], bool)]) -> Vec<u8> {
    let mut out = Vec::new();
    let mut cen = Vec::new();
    for (name, data, deflate) in entries {
        let mut crc = Crc::new();
        crc.update(data);
        let (method, body) = if *deflate {
            let mut e = DeflateEncoder::new(Vec::new(), flate2::Compression::default());
            e.write_all(data).unwrap();
            (8u16, e.finish().unwrap())
        } else {
            (0u16, data.to_vec())
        };
        let off = out.len() as u32;
        out.extend_from_slice(&0x0403_4b50u32.to_le_bytes());
        out.extend_from_slice(&[20, 0, 0, 0]);
        out.extend_from_slice(&method.to_le_bytes());
        out.extend_from_slice(&[0, 0, 0x21, 0]);
        out.extend_from_slice(&crc.sum().to_le_bytes());
        out.extend_from_slice(&(body.len() as u32).to_le_bytes());
        out.extend_from_slice(&(data.len() as u32).to_le_bytes());
        out.extend_from_slice(&(name.len() as u16).to_le_bytes());
        out.extend_from_slice(&[0, 0]);
        out.extend_from_slice(name.as_bytes());
        out.extend_from_slice(&body);

        cen.extend_from_slice(&0x0201_4b50u32.to_le_bytes());
        cen.extend_from_slice(&[20, 0, 20, 0, 0, 0]);
        cen.extend_from_slice(&method.to_le_bytes());
        cen.extend_from_slice(&[0, 0, 0x21, 0]);
        cen.extend_from_slice(&crc.sum().to_le_bytes());
        cen.extend_from_slice(&(body.len() as u32).to_le_bytes());
        cen.extend_from_slice(&(data.len() as u32).to_le_bytes());
        cen.extend_from_slice(&(name.len() as u16).to_le_bytes());
        cen.extend_from_slice(&[0u8; 12]);
        cen.extend_from_slice(&off.to_le_bytes());
        cen.extend_from_slice(name.as_bytes());
    }
    let cen_off = out.len() as u32;
    out.extend_from_slice(&cen);
    out.extend_from_slice(&0x0605_4b50u32.to_le_bytes());
    out.extend_from_slice(&[0, 0, 0, 0]);
    out.extend_from_slice(&(entries.len() as u16).to_le_bytes());
    out.extend_from_slice(&(entries.len() as u16).to_le_bytes());
    out.extend_from_slice(&(cen.len() as u32).to_le_bytes());
    out.extend_from_slice(&cen_off.to_le_bytes());
    out.extend_from_slice(&[0, 0]);
    out
}

pub enum Node {
    File(String, Vec<u8>),
    Dir(String, Vec<Node>),
}

pub fn file(n: &str, d: &[u8]) -> Node {
    Node::File(n.into(), d.to_vec())
}
pub fn dir(n: &str, c: Vec<Node>) -> Node {
    Node::Dir(n.into(), c)
}

fn record(name: &[u8], lba: u32, len: u32, is_dir: bool) -> Vec<u8> {
    let mut l = 33 + name.len();
    if l % 2 == 1 {
        l += 1;
    }
    let mut r = vec![0u8; l];
    r[0] = l as u8;
    r[2..6].copy_from_slice(&lba.to_le_bytes());
    r[6..10].copy_from_slice(&lba.to_be_bytes());
    r[10..14].copy_from_slice(&len.to_le_bytes());
    r[14..18].copy_from_slice(&len.to_be_bytes());
    r[25] = if is_dir { 2 } else { 0 };
    r[32] = name.len() as u8;
    r[33..33 + name.len()].copy_from_slice(name);
    r
}

struct Builder {
    sectors: Vec<Vec<u8>>, // index = lba - 18
}

impl Builder {
    fn alloc(&mut self) -> u32 {
        self.sectors.push(Vec::new());
        (self.sectors.len() - 1) as u32 + 18
    }
    fn put_dir(&mut self, own: u32, parent: u32, kids: &[Node]) {
        let mut recs: Vec<u8> = Vec::new();
        recs.extend(record(&[0], own, 2048, true));
        recs.extend(record(&[1], parent, 2048, true));
        for k in kids {
            match k {
                Node::File(n, d) => {
                    let lba = self.alloc();
                    let chunks: Vec<&[u8]> = d.chunks(2048).collect();
                    self.sectors[(lba - 18) as usize] = chunks.first().copied().unwrap_or(&[]).to_vec();
                    for c in chunks.iter().skip(1) {
                        let l = self.alloc();
                        self.sectors[(l - 18) as usize] = c.to_vec();
                    }
                    let id = format!("{};1", n.to_ascii_uppercase());
                    recs.extend(record(id.as_bytes(), lba, d.len() as u32, false));
                }
                Node::Dir(n, c) => {
                    let lba = self.alloc();
                    self.put_dir(lba, own, c);
                    recs.extend(record(n.to_ascii_uppercase().as_bytes(), lba, 2048, true));
                }
            }
        }
        assert!(recs.len() <= 2048, "fixture dir too big");
        recs.resize(2048, 0);
        self.sectors[(own - 18) as usize] = recs;
    }
}

/// Builds a one-sector-per-directory ISO 9660 image.
pub fn make_iso(root: Vec<Node>) -> Vec<u8> {
    let mut b = Builder { sectors: Vec::new() };
    let root_lba = b.alloc();
    b.put_dir(root_lba, root_lba, &root);
    let mut img = vec![0u8; 16 * 2048];
    let mut pvd = vec![0u8; 2048];
    pvd[0] = 1;
    pvd[1..6].copy_from_slice(b"CD001");
    pvd[6] = 1;
    pvd[128..130].copy_from_slice(&2048u16.to_le_bytes());
    pvd[156..190].copy_from_slice(&record(&[0], root_lba, 2048, true)[..34]);
    img.extend(pvd);
    let mut term = vec![0u8; 2048];
    term[0] = 255;
    term[1..6].copy_from_slice(b"CD001");
    img.extend(term);
    for s in b.sectors {
        let mut s = s;
        s.resize(2048, 0);
        img.extend(s);
    }
    img
}

/// A synthetic Live CD: kernel, command.com and two package sets with a few members.
pub fn fake_livecd() -> Vec<u8> {
    let bin = make_zip(&[
        ("BIN/HELLO.COM", b"hello-com", true),
        ("BIN/HIMEMX.EXE", b"himemx", false),
        ("DOC/HELLO/README.TXT", b"readme", true),
        ("APPINFO/HELLO.LSM", b"lsm", false),
        ("SOURCE/HELLO/HELLO.ASM", b"source-must-be-dropped", false),
    ]);
    let net = make_zip(&[("BIN/PING.EXE", b"ping", false)]);
    make_iso(vec![
        file("KERNEL.SYS", b"KERNEL-BYTES"),
        file("COMMAND.COM", b"COMMAND-BYTES"),
        dir(
            "packages",
            vec![
                dir("base", vec![file("hello.zip", &bin)]),
                dir("net", vec![file("ping.zip", &net)]),
            ],
        ),
    ])
}
