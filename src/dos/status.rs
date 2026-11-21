//! `bilterm dos status` / `:dos status` / `dos.status`: one read-only view model (UNI-R01).
//! Text for the CLI and colon, JSON for RPC/MCP, both rendered from [`Status`].

use super::backend::probe_qemu;
use super::cli::Ctx;
use super::image::store::ImageStore;
use crate::api::json::JsonValue;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ImageRow {
    pub name: String,
    pub snapshots: usize,
    /// Pid of the live `dos run` holding the image lock.
    pub in_use_by: Option<u32>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Status {
    pub data_dir: Option<String>,
    pub emulator_found: bool,
    pub emulator_usable: bool,
    pub emulator_version: Option<String>,
    pub emulator_detail: String,
    pub downloads: Vec<String>,
    pub boot_sector_cached: bool,
    pub images: Vec<ImageRow>,
}

pub fn gather(ctx: &Ctx) -> Status {
    let bp = probe_qemu(ctx.probe, ctx.manifest.min_version());
    let mut st = Status {
        data_dir: ctx.paths.as_ref().map(|p| p.root().display().to_string()),
        emulator_found: bp.found,
        emulator_usable: bp.usable(),
        emulator_version: bp.version.map(|v| v.to_string()),
        emulator_detail: bp.detail,
        downloads: Vec::new(),
        boot_sector_cached: false,
        images: Vec::new(),
    };
    let Some(paths) = ctx.paths.as_ref() else { return st };
    let mut dl: Vec<String> = std::fs::read_dir(paths.downloads_dir())
        .into_iter()
        .flatten()
        .flatten()
        .filter_map(|e| e.file_name().to_str().map(str::to_string))
        .filter(|n| !n.ends_with(".part"))
        .collect();
    dl.sort();
    st.downloads = dl;
    st.boot_sector_cached = paths.boot_sector_path().is_file();
    let store = ImageStore::new(paths.clone());
    st.images = store
        .image_names()
        .into_iter()
        .map(|name| ImageRow {
            snapshots: store.list(&name).map(|v| v.len()).unwrap_or(0),
            in_use_by: store.is_locked(&name, ctx.procs).ok().flatten(),
            name,
        })
        .collect();
    st
}

pub fn render(st: &Status) -> String {
    let mut o = String::new();
    o.push_str(&format!("data dir: {}\n", st.data_dir.as_deref().unwrap_or("(unknown)")));
    o.push_str(&format!(
        "emulator: {}\n",
        match (st.emulator_found, st.emulator_usable) {
            (false, _) => "not found".to_string(),
            (true, true) => format!("ok {}", st.emulator_version.as_deref().unwrap_or("")),
            (true, false) => format!("unusable ({})", st.emulator_detail),
        }
    ));
    o.push_str(&format!("downloads: {}\n", if st.downloads.is_empty() { "none".into() } else { st.downloads.join(", ") }));
    o.push_str(&format!("boot sector cached: {}\n", if st.boot_sector_cached { "yes" } else { "no" }));
    if st.images.is_empty() {
        o.push_str("images: none (run `dos setup`)\n");
    }
    for i in &st.images {
        let busy = i.in_use_by.map(|p| format!(", in use by pid {p}")).unwrap_or_default();
        o.push_str(&format!("image {}: {} snapshot(s){busy}\n", i.name, i.snapshots));
    }
    o
}

pub fn to_json(st: &Status) -> JsonValue {
    let s = |v: &str| JsonValue::String(v.to_string());
    JsonValue::obj([
        ("data_dir", st.data_dir.as_deref().map_or(JsonValue::Null, s)),
        (
            "emulator",
            JsonValue::obj([
                ("found", JsonValue::Bool(st.emulator_found)),
                ("usable", JsonValue::Bool(st.emulator_usable)),
                ("version", st.emulator_version.as_deref().map_or(JsonValue::Null, s)),
                ("detail", s(&st.emulator_detail)),
            ]),
        ),
        ("downloads", JsonValue::Array(st.downloads.iter().map(|d| s(d)).collect())),
        ("boot_sector_cached", JsonValue::Bool(st.boot_sector_cached)),
        (
            "images",
            JsonValue::Array(
                st.images
                    .iter()
                    .map(|i| {
                        JsonValue::obj([
                            ("name", s(&i.name)),
                            ("snapshots", JsonValue::Number(i.snapshots as i64)),
                            ("in_use_by", i.in_use_by.map_or(JsonValue::Null, |p| JsonValue::Number(i64::from(p)))),
                        ])
                    })
                    .collect(),
            ),
        ),
    ])
}
