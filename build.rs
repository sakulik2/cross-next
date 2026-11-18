//! 给 `desktop.exe` 嵌入版本信息与程序图标。
//!
//! 任务管理器「进程」页和任务栏按钮的右键菜单显示的是版本信息里的
//! `FileDescription`，没有它就退回 exe 文件名 —— 小组件会显示成「desktop.exe」，
//! 看不出是什么。资源管理器和任务管理器里的图标则来自 exe 自带的图标资源。
//!
//! 不引入 `embed-resource`/`winres`：它们要找 Windows SDK 里的 `rc.exe`。`.res` 的
//! 二进制格式很简单，这里直接写出来，`link.exe` 认 `.res` 输入，会自己转成 COFF。

use std::path::Path;

const ICO: &str = "assets/tray.ico";

fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-changed={ICO}");

    // 只有 MSVC 的链接器吃 .res；别的目标（比如有人在 Linux 上跑 clippy）跳过即可。
    if std::env::var("CARGO_CFG_TARGET_ENV").as_deref() != Ok("msvc") {
        return;
    }

    let version = std::env::var("CARGO_PKG_VERSION").unwrap();
    let ico = std::fs::read(ICO).expect("读取图标");

    let mut res = Res::new();
    add_icon(&mut res, &ico);
    res.add(
        RT_VERSION,
        1,
        &version_info(&version, "cross-next", "desktop.exe"),
    );

    let out = Path::new(&std::env::var("OUT_DIR").unwrap()).join("desktop.res");
    std::fs::write(&out, res.bytes).expect("写 .res");
    println!("cargo:rustc-link-arg-bin=desktop={}", out.display());
}

const RT_ICON: u16 = 3;
const RT_GROUP_ICON: u16 = 14;
const RT_VERSION: u16 = 16;
/// 简体中文。语言只影响资源的归类，不影响显示。
const LANG: u16 = 0x0804;
/// UTF-16，StringFileInfo 的代码页。
const CODEPAGE: u16 = 1200;

/// `.res` 文件：一个空的头条目，后面每个资源一条「头 + 数据」，都按 4 字节对齐。
struct Res {
    bytes: Vec<u8>,
}

impl Res {
    fn new() -> Res {
        let mut r = Res { bytes: Vec::new() };
        // 规定的开头：类型与名称都是序号 0、数据长度 0 的空条目，标明这是 32 位 .res。
        r.entry(0, 0, 0, 0, &[]);
        r
    }

    fn add(&mut self, kind: u16, id: u16, data: &[u8]) {
        // 0x1030 = MOVEABLE | PURE | DISCARDABLE，rc.exe 的默认值。
        self.entry(kind, id, 0x1030, LANG, data);
    }

    fn entry(&mut self, kind: u16, id: u16, flags: u16, lang: u16, data: &[u8]) {
        let b = &mut self.bytes;
        b.extend((data.len() as u32).to_le_bytes()); // DataSize
        b.extend(32u32.to_le_bytes()); // HeaderSize：类型、名称都用序号时固定 32
        b.extend([0xFF, 0xFF]);
        b.extend(kind.to_le_bytes());
        b.extend([0xFF, 0xFF]);
        b.extend(id.to_le_bytes());
        b.extend(0u32.to_le_bytes()); // DataVersion
        b.extend(flags.to_le_bytes());
        b.extend(lang.to_le_bytes());
        b.extend(0u32.to_le_bytes()); // Version
        b.extend(0u32.to_le_bytes()); // Characteristics
        b.extend(data);
        pad4(b);
    }
}

fn pad4(b: &mut Vec<u8>) {
    while !b.len().is_multiple_of(4) {
        b.push(0);
    }
}

fn u16_at(b: &[u8], i: usize) -> u16 {
    u16::from_le_bytes([b[i], b[i + 1]])
}

fn u32_at(b: &[u8], i: usize) -> u32 {
    u32::from_le_bytes([b[i], b[i + 1], b[i + 2], b[i + 3]])
}

/// `.ico` → 每张图一个 `RT_ICON`，外加一个列出它们的 `RT_GROUP_ICON`。
///
/// 组里每项和 `.ico` 目录项几乎一样（见 `icon::pick_image`），只是末尾的 4 字节
/// 文件偏移换成 2 字节的 `RT_ICON` 序号，所以每项 14 字节而不是 16。
fn add_icon(res: &mut Res, ico: &[u8]) {
    let count = u16_at(ico, 4) as usize;
    let mut group = Vec::new();
    group.extend(&ico[..6]); // reserved、type=1、count，原样照抄
    for i in 0..count {
        let e = 6 + i * 16;
        let size = u32_at(ico, e + 8) as usize;
        let offset = u32_at(ico, e + 12) as usize;
        let id = i as u16 + 1;
        res.add(RT_ICON, id, &ico[offset..offset + size]);
        group.extend(&ico[e..e + 12]); // 宽、高、色数、保留、planes、位深、字节数
        group.extend(id.to_le_bytes());
    }
    // 资源管理器取 exe 里第一个图标组当程序图标。
    res.add(RT_GROUP_ICON, 1, &group);
}

/// `VS_VERSIONINFO`。整棵树由同一种节点组成：
/// `wLength, wValueLength, wType, szKey（UTF-16 带结尾 0）, 对齐, Value, 对齐, 子节点…`。
fn version_info(version: &str, description: &str, filename: &str) -> Vec<u8> {
    let mut parts = version.split('.').map(|p| p.parse::<u32>().unwrap_or(0));
    let (major, minor, patch) = (
        parts.next().unwrap_or(0),
        parts.next().unwrap_or(0),
        parts.next().unwrap_or(0),
    );
    let ms = (major << 16) | minor;
    let ls = patch << 16;

    let mut fixed = Vec::new();
    for v in [
        0xFEEF04BD, // 签名
        0x0001_0000,
        ms, // 文件版本
        ls,
        ms, // 产品版本
        ls,
        0x3F,        // FileFlagsMask
        0,           // FileFlags
        0x0004_0004, // VOS_NT_WINDOWS32
        1,           // VFT_APP
        0,
        0,
        0,
    ] {
        fixed.extend(u32::to_le_bytes(v));
    }

    let strings = [
        ("FileDescription", description),
        ("ProductName", "cross-next"),
        ("FileVersion", version),
        ("ProductVersion", version),
        ("OriginalFilename", filename),
        ("InternalName", filename.trim_end_matches(".exe")),
    ];
    let table = node(
        &format!("{LANG:04x}{CODEPAGE:04x}"),
        1,
        &[],
        0,
        strings
            .iter()
            .map(|(k, v)| {
                let value = utf16z(v);
                // 字符串节点的 wValueLength 按字符（含结尾 0）计，不是字节。
                node(k, 1, &value, value.len() as u16 / 2, vec![])
            })
            .collect(),
    );
    let string_info = node("StringFileInfo", 1, &[], 0, vec![table]);

    let mut translation = Vec::new();
    translation.extend(LANG.to_le_bytes());
    translation.extend(CODEPAGE.to_le_bytes());
    let var = node("Translation", 0, &translation, 4, vec![]);
    let var_info = node("VarFileInfo", 1, &[], 0, vec![var]);

    node(
        "VS_VERSION_INFO",
        0,
        &fixed,
        fixed.len() as u16,
        vec![string_info, var_info],
    )
}

fn node(key: &str, kind: u16, value: &[u8], value_len: u16, children: Vec<Vec<u8>>) -> Vec<u8> {
    let mut b = vec![0, 0]; // wLength，最后回填
    b.extend(value_len.to_le_bytes());
    b.extend(kind.to_le_bytes());
    b.extend(utf16z(key));
    pad4(&mut b);
    b.extend(value);
    for c in children {
        pad4(&mut b);
        b.extend(c);
    }
    let len = b.len() as u16;
    b[..2].copy_from_slice(&len.to_le_bytes());
    b
}

fn utf16z(s: &str) -> Vec<u8> {
    s.encode_utf16()
        .chain([0])
        .flat_map(u16::to_le_bytes)
        .collect()
}
