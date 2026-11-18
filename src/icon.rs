//! 内嵌 .ico 的解析：托盘图标与 `desktop.exe` 的按钮图标共用。
//!
//! 走手解 `ICONDIR` 这条路是为了不引入 winres 之类的构建期依赖 —— 图标都是
//! `tools/make-icon.mjs` 生成、`include_bytes!` 编进 exe 的。

use windows::Win32::UI::WindowsAndMessaging::{CreateIconFromResourceEx, HICON, IMAGE_FLAGS};

/// 从 .ico 里挑宽度最接近 `want` 的一张，造出 `size`×`size` 的 `HICON`。
///
/// `CreateIconFromResourceEx` 要的是**单张图**的资源位，不是带目录头的 .ico 文件 ——
/// 整个文件喂进去会失败。所以先用 [`pick_image`] 切出单张。
pub fn load(ico: &[u8], size: i32) -> Option<HICON> {
    let bits = pick_image(ico, size)?;
    // dwVer 固定 0x00030000 —— 这是 .ico 资源格式的版本号，不是系统版本。
    unsafe { CreateIconFromResourceEx(bits, true, 0x0003_0000, size, size, IMAGE_FLAGS(0)) }.ok()
}

/// 从 .ico 的字节里切出宽度最接近 `want` 的那张图。
///
/// 单独拎出来是为了可测：字节偏移算错不会报错，只会让图标神秘地不出现，
/// 而那要跑起来才看得见。下面的测试直接拿真实的 `assets/*.ico` 验。
///
/// 所有取值都走 `get`，畸形文件返回 `None` 而不是 panic —— 这些字节数组是编进
/// exe 的，不该崩，但真崩了就把整个界面搭进去了。
pub fn pick_image(ico: &[u8], want: i32) -> Option<&[u8]> {
    // 同样接近时取大的：缩小比放大清楚。
    let (_, off, len) = entries(ico)?
        .into_iter()
        .min_by_key(|(w, ..)| ((w - want).abs(), -w))?;
    ico.get(off..off.checked_add(len)?)
}

/// 切出宽度**不小于** `want` 的最小一张；都比它小就取最大的。
///
/// 给要自己缩放的场合用（[`decode_bgra`] 之后再缩）：放大会把抗锯齿的边缘
/// 放成台阶，比如 48 放到 72 时圆角明显发毛，而往下缩总是干净的。
/// `HICON` 走 [`pick_image`] 即可，系统画图标时自己会处理。
pub fn pick_at_least(ico: &[u8], want: i32) -> Option<&[u8]> {
    let all = entries(ico)?;
    let (_, off, len) = all
        .iter()
        .filter(|(w, ..)| *w >= want)
        .min_by_key(|(w, ..)| *w)
        .or_else(|| all.iter().max_by_key(|(w, ..)| *w))
        .copied()?;
    ico.get(off..off.checked_add(len)?)
}

/// 目录里每张图的 (宽, 偏移, 长度)。目录项残缺时返回 `None`。
fn entries(ico: &[u8]) -> Option<Vec<(i32, usize, usize)>> {
    let count = u16::from_le_bytes(ico.get(4..6)?.try_into().ok()?) as usize;
    (0..count)
        .map(|i| {
            let e = ico.get(6 + i * 16..6 + i * 16 + 16)?;
            // 宽度字段只有一个字节，0 表示 256。
            let width = if e[0] == 0 { 256 } else { e[0] as i32 };
            let len = u32::from_le_bytes(e[8..12].try_into().ok()?) as usize;
            let off = u32::from_le_bytes(e[12..16].try_into().ok()?) as usize;
            Some((width, off, len))
        })
        .collect()
}

/// 把 [`pick_image`] 切出的单张 32 位图解成自上而下的预乘 BGRA。
///
/// 给需要自己做混合的场合用（`desktop.exe` 画按钮时要半透明、要缩放，`HICON`
/// 做不到）。.ico 里的位图是**自下而上**存的，且高度字段是 XOR + AND 两块之和，
/// 这两点都得在这里抹平。非 32 位的图返回 `None` —— 生成脚本只产 32 位。
pub fn decode_bgra(bits: &[u8]) -> Option<(usize, Vec<u8>)> {
    let header = u32::from_le_bytes(bits.get(0..4)?.try_into().ok()?) as usize;
    let w = i32::from_le_bytes(bits.get(4..8)?.try_into().ok()?);
    let h2 = i32::from_le_bytes(bits.get(8..12)?.try_into().ok()?);
    let bpp = u16::from_le_bytes(bits.get(14..16)?.try_into().ok()?);
    if bpp != 32 || w <= 0 || h2 != w * 2 {
        return None;
    }
    let side = w as usize;
    let stride = side * 4;
    let pixels = bits.get(header..header.checked_add(stride * side)?)?;

    let mut out = vec![0u8; stride * side];
    for row in 0..side {
        let src = &pixels[(side - 1 - row) * stride..][..stride];
        let dst = &mut out[row * stride..][..stride];
        for (s, d) in src
            .as_chunks::<4>()
            .0
            .iter()
            .zip(dst.as_chunks_mut::<4>().0.iter_mut())
        {
            let a = s[3] as u32;
            // 预乘：后续的混合按预乘 alpha 算（AlphaBlend 也要求这样）。
            d[0] = (s[0] as u32 * a / 255) as u8;
            d[1] = (s[1] as u32 * a / 255) as u8;
            d[2] = (s[2] as u32 * a / 255) as u8;
            d[3] = s[3];
        }
    }
    Some((side, out))
}

#[cfg(test)]
mod tests {
    use super::*;

    const TRAY: &[u8] = include_bytes!("../assets/tray.ico");

    /// 内嵌的图标必须是能解出图来的 —— 换图标或改生成脚本时这条会兜住。
    #[test]
    fn embedded_icon_parses() {
        for want in [16, 20, 24, 32, 48] {
            let bits = pick_image(TRAY, want).expect("应能选出一张图");
            // BITMAPINFOHEADER 的 biSize 恒为 40，用它确认切的位置对齐到了图头
            // 而不是文件中间某处。
            assert_eq!(u32::from_le_bytes(bits[0..4].try_into().unwrap()), 40);
            // biHeight 是 XOR + AND 两块之和，所以应是宽的两倍。
            let w = i32::from_le_bytes(bits[4..8].try_into().unwrap());
            let h = i32::from_le_bytes(bits[8..12].try_into().unwrap());
            assert_eq!(h, w * 2, "{want}px 这张的高度字段不对");
        }
    }

    /// 16 和 32 两档要真的分得开，别都落到同一张上。
    #[test]
    fn picks_nearest_size() {
        let small = pick_image(TRAY, 16).unwrap();
        let large = pick_image(TRAY, 32).unwrap();
        assert_eq!(i32::from_le_bytes(small[4..8].try_into().unwrap()), 16);
        assert_eq!(i32::from_le_bytes(large[4..8].try_into().unwrap()), 32);
    }

    /// 解出来的图要自上而下：托盘图标是「蓝底圆角方块」，四角透明、中心不透明，
    /// 上下翻没翻对靠这个分不出来，所以另看渐变 —— 左上浅、右下深。
    #[test]
    fn decodes_top_down_premultiplied() {
        let (side, px) = decode_bgra(pick_image(TRAY, 32).unwrap()).unwrap();
        assert_eq!(side, 32);
        assert_eq!(px.len(), 32 * 32 * 4);
        assert_eq!(px[3], 0, "左上角应透明");
        let at = |x: usize, y: usize| &px[(y * side + x) * 4..][..4];
        assert_eq!(at(16, 16)[3], 255, "中心应不透明");
        // 渐变：左上的红通道比右下亮（C0 = 0x7a，C1 = 0x2a）。避开音符取点。
        assert!(at(5, 5)[2] > at(27, 27)[2], "上下翻反了");
        // 预乘后任何通道都不会超过 alpha。
        assert!(
            px.as_chunks::<4>()
                .0
                .iter()
                .all(|p| p[0] <= p[3] && p[1] <= p[3] && p[2] <= p[3])
        );
    }

    /// 要 72 时取 96 往下缩，而不是取 64 往上放 —— 这就是锯齿那次的修法。
    #[test]
    fn at_least_never_upscales() {
        let width = |bits: &[u8]| i32::from_le_bytes(bits[4..8].try_into().unwrap());
        assert_eq!(width(pick_at_least(TRAY, 72).unwrap()), 96);
        assert_eq!(width(pick_at_least(TRAY, 48).unwrap()), 48);
        assert_eq!(width(pick_at_least(TRAY, 1).unwrap()), 16);
        // 比最大的还大：只能取最大的。
        assert_eq!(width(pick_at_least(TRAY, 999).unwrap()), 128);
        // 对照：最接近的那张是 64，放大上去正是锯齿的来源。
        assert_eq!(width(pick_image(TRAY, 72).unwrap()), 64);
    }

    /// 畸形输入返回 None，不 panic —— 越界读会把整个进程带走。
    #[test]
    fn malformed_is_none() {
        assert!(pick_image(&[], 16).is_none());
        assert!(pick_image(&[0, 0, 1, 0], 16).is_none()); // 头被截断
        // 声称有一张图，但目录项不全
        assert!(pick_image(&[0, 0, 1, 0, 1, 0, 16, 16], 16).is_none());
        // 目录项完整但偏移越界
        let mut bad = vec![0, 0, 1, 0, 1, 0];
        bad.extend_from_slice(&[16, 16, 0, 0, 1, 0, 32, 0]);
        bad.extend_from_slice(&999u32.to_le_bytes()); // 长度
        bad.extend_from_slice(&999u32.to_le_bytes()); // 偏移
        assert!(pick_image(&bad, 16).is_none());
        assert!(decode_bgra(&[40, 0, 0, 0]).is_none());
    }
}
