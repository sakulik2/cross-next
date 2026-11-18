//! 纯 Rust 的像素运算：缩放、裁切、圆角、混合、取色。
//!
//! 小组件的画面全在内存里拼好（底色、封面、按钮），GDI 只负责写字。这样做是因为
//! GDI 画不出抗锯齿的形状、也做不了半透明叠加，而这些在一块 BGRA 缓冲上几行就够。
//! 全部是纯函数，能在 `cargo test` 里验。
//!
//! 约定：BGRA、自上而下、**预乘 alpha** —— 与 `AlphaBlend`/DWM 缩略图的要求一致。

/// sRGB 颜色，RGB 顺序。
pub type Rgb = [u8; 3];

#[derive(Clone, Debug, PartialEq)]
pub struct Image {
    pub w: usize,
    pub h: usize,
    pub px: Vec<u8>,
}

impl Image {
    /// 全透明。
    pub fn new(w: usize, h: usize) -> Image {
        Image {
            w,
            h,
            px: vec![0; w * h * 4],
        }
    }

    #[cfg(test)]
    pub fn at(&self, x: usize, y: usize) -> [u8; 4] {
        let i = (y * self.w + x) * 4;
        [self.px[i], self.px[i + 1], self.px[i + 2], self.px[i + 3]]
    }

    /// 铺底色：`b` 打底，以 (cx, cy) 为中心晕开一团 `a`。
    ///
    /// 对应网页背景的 radial-gradient 光晕。坐标与半径都是像素。
    pub fn glow(&mut self, a: Rgb, b: Rgb, cx: f32, cy: f32, radius: f32) {
        for y in 0..self.h {
            for x in 0..self.w {
                let d = ((x as f32 - cx).powi(2) + (y as f32 - cy).powi(2)).sqrt();
                let t = (1.0 - d / radius).clamp(0.0, 1.0);
                let t = t * t * (3.0 - 2.0 * t); // smoothstep，边缘不出硬圈
                let i = (y * self.w + x) * 4;
                for k in 0..3 {
                    let v = b[k] as f32 + (a[k] as f32 - b[k] as f32) * t;
                    self.px[i + 2 - k] = v.round() as u8;
                }
                self.px[i + 3] = 255;
            }
        }
    }

    /// 把 `src` 以 `opacity`（0-255）叠到 (x, y)，越界部分裁掉。预乘 alpha 的 over 运算。
    pub fn blend(&mut self, src: &Image, x: i32, y: i32, opacity: u8) {
        let op = opacity as u32;
        for sy in 0..src.h {
            let dy = y + sy as i32;
            if dy < 0 || dy >= self.h as i32 {
                continue;
            }
            for sx in 0..src.w {
                let dx = x + sx as i32;
                if dx < 0 || dx >= self.w as i32 {
                    continue;
                }
                let s = (sy * src.w + sx) * 4;
                let d = (dy as usize * self.w + dx as usize) * 4;
                let sa = src.px[s + 3] as u32 * op / 255;
                if sa == 0 {
                    continue;
                }
                for k in 0..4 {
                    let sv = src.px[s + k] as u32 * op / 255;
                    let dv = self.px[d + k] as u32;
                    self.px[d + k] = (sv + dv * (255 - sa) / 255).min(255) as u8;
                }
            }
        }
    }

    /// 以纯色 `rgb`、不透明度 `alpha` 叠一个矩形。用于进度条。
    pub fn fill(&mut self, x: i32, y: i32, w: usize, h: usize, rgb: Rgb, alpha: u8) {
        let mut patch = Image::new(w, h);
        for p in patch.px.as_chunks_mut::<4>().0.iter_mut() {
            p.copy_from_slice(&[rgb[2], rgb[1], rgb[0], 255]);
        }
        self.blend(&patch, x, y, alpha);
    }

    /// 居中裁成正方形。封面一般本来就是方的，但 SMTC 不保证。
    pub fn crop_square(&self) -> Image {
        let side = self.w.min(self.h);
        let (ox, oy) = ((self.w - side) / 2, (self.h - side) / 2);
        let mut out = Image::new(side, side);
        for y in 0..side {
            let src = ((oy + y) * self.w + ox) * 4;
            out.px[y * side * 4..(y + 1) * side * 4].copy_from_slice(&self.px[src..src + side * 4]);
        }
        out
    }

    /// 缩放到 `w`×`h`。缩小按面积平均（不出摩尔纹），放大按双线性。
    pub fn resize(&self, w: usize, h: usize) -> Image {
        if (w, h) == (self.w, self.h) {
            return self.clone();
        }
        if w == 0 || h == 0 || self.w == 0 || self.h == 0 {
            return Image::new(w, h);
        }
        let wx = weights(self.w, w);
        let wy = weights(self.h, h);

        // 先横后纵，中间结果用 f32 免得两次取整。
        let mut mid = vec![0f32; w * self.h * 4];
        for y in 0..self.h {
            for (x, taps) in wx.iter().enumerate() {
                let mut acc = [0f32; 4];
                for &(sx, wt) in taps {
                    let s = (y * self.w + sx) * 4;
                    for (k, a) in acc.iter_mut().enumerate() {
                        *a += self.px[s + k] as f32 * wt;
                    }
                }
                mid[(y * w + x) * 4..][..4].copy_from_slice(&acc);
            }
        }
        let mut out = Image::new(w, h);
        for (y, taps) in wy.iter().enumerate() {
            for x in 0..w {
                let mut acc = [0f32; 4];
                for &(sy, wt) in taps {
                    let s = (sy * w + x) * 4;
                    for (k, a) in acc.iter_mut().enumerate() {
                        *a += mid[s + k] * wt;
                    }
                }
                for (k, a) in acc.iter().enumerate() {
                    out.px[(y * w + x) * 4 + k] = a.round().clamp(0.0, 255.0) as u8;
                }
            }
        }
        out
    }

    /// 圆角：按每个像素被圆角覆盖的比例压低 alpha（预乘所以四个通道一起压）。
    /// 解析算覆盖率，边缘是抗锯齿的 —— GDI 的 RoundRect 做不到这一点。
    pub fn round_corners(&mut self, r: f32) {
        for y in 0..self.h {
            for x in 0..self.w {
                // 像素中心到最近那个圆角圆心的距离；不在角区的距离为 0。
                let px = x as f32 + 0.5;
                let py = y as f32 + 0.5;
                let dx = (r - px).max(px - (self.w as f32 - r)).max(0.0);
                let dy = (r - py).max(py - (self.h as f32 - r)).max(0.0);
                let d = (dx * dx + dy * dy).sqrt();
                let cover = (r - d + 0.5).clamp(0.0, 1.0);
                if cover < 1.0 {
                    let i = (y * self.w + x) * 4;
                    for k in 0..4 {
                        self.px[i + k] = (self.px[i + k] as f32 * cover).round() as u8;
                    }
                }
            }
        }
    }
}

/// 抗锯齿的实心圆，按钮悬停时的底。
pub fn disc(size: usize, rgb: Rgb) -> Image {
    let mut img = Image::new(size, size);
    for p in img.px.as_chunks_mut::<4>().0.iter_mut() {
        p.copy_from_slice(&[rgb[2], rgb[1], rgb[0], 255]);
    }
    img.round_corners(size as f32 / 2.0);
    img
}

/// 一维重采样权重：每个目标像素取哪些源像素、各占多少。
fn weights(src: usize, dst: usize) -> Vec<Vec<(usize, f32)>> {
    let scale = src as f32 / dst as f32;
    (0..dst)
        .map(|i| {
            if scale >= 1.0 {
                // 缩小：目标像素覆盖源上 [lo, hi) 这一段，按重叠长度加权。
                let lo = i as f32 * scale;
                let hi = lo + scale;
                let mut taps = Vec::new();
                let mut s = lo.floor() as usize;
                while (s as f32) < hi && s < src {
                    let overlap = (hi.min(s as f32 + 1.0) - lo.max(s as f32)).max(0.0);
                    if overlap > 0.0 {
                        taps.push((s, overlap / scale));
                    }
                    s += 1;
                }
                taps
            } else {
                // 放大：双线性，取左右两个最近的源像素。
                let c = ((i as f32 + 0.5) * scale - 0.5).clamp(0.0, (src - 1) as f32);
                let l = c.floor() as usize;
                let r = (l + 1).min(src - 1);
                let t = c - l as f32;
                if l == r {
                    vec![(l, 1.0)]
                } else {
                    vec![(l, 1.0 - t), (r, t)]
                }
            }
        })
        .collect()
}

/// 从封面取一对配色 `(亮部, 底色)`，取不出来返回 `None`（用默认配色）。
///
/// 照搬网页的 `pickColors`：缩到 16×16，丢掉太灰、太暗、太亮的像素，按 32 级量化
/// 投票，取票最多那一桶的平均色。灰度像素不投票是为了暗色封面不会平均成一团泥。
pub fn pick_colors(cover: &Image) -> Option<(Rgb, Rgb)> {
    let small = cover.resize(16, 16);
    let mut buckets = std::collections::HashMap::<u32, [u32; 4]>::new();
    for p in small.px.as_chunks::<4>().0.iter() {
        if p[3] < 128 {
            continue;
        }
        let (r, g, b) = (p[2] as u32, p[1] as u32, p[0] as u32);
        let max = r.max(g).max(b);
        let min = r.min(g).min(b);
        if max - min < 28 || max < 45 || min > 225 {
            continue;
        }
        let key = (r >> 5) * 64 + (g >> 5) * 8 + (b >> 5);
        let acc = buckets.entry(key).or_default();
        acc[0] += r;
        acc[1] += g;
        acc[2] += b;
        acc[3] += 1;
    }
    // 票数相同时按桶号取，结果才确定 —— HashMap 的遍历顺序不固定。
    let top = buckets
        .iter()
        .max_by_key(|(k, v)| (v[3], std::cmp::Reverse(**k)))?
        .1;
    let avg = [top[0] / top[3], top[1] / top[3], top[2] / top[3]];

    // 主色调暗一档做光晕，再压得更暗做底色，保证白字的对比度始终够。
    let mix = |v: u32, t: u32, k: f32| (v as f32 * (1.0 - k) + t as f32 * k).round() as u8;
    let a = [
        mix(avg[0], 0, 0.18),
        mix(avg[1], 0, 0.18),
        mix(avg[2], 0, 0.18),
    ];
    let b = [
        mix(avg[0], 8, 0.84),
        mix(avg[1], 8, 0.84),
        mix(avg[2], 10, 0.84),
    ];
    Some((a, b))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn solid(w: usize, h: usize, rgb: Rgb) -> Image {
        let mut img = Image::new(w, h);
        for p in img.px.as_chunks_mut::<4>().0.iter_mut() {
            p.copy_from_slice(&[rgb[2], rgb[1], rgb[0], 255]);
        }
        img
    }

    #[test]
    fn resize_preserves_solid_color_both_ways() {
        let img = solid(7, 5, [200, 100, 50]);
        for (w, h) in [(3, 2), (16, 11), (1, 1), (7, 5)] {
            let out = img.resize(w, h);
            assert_eq!((out.w, out.h), (w, h));
            assert!(
                out.px
                    .as_chunks::<4>()
                    .0
                    .iter()
                    .all(|p| *p == [50, 100, 200, 255]),
                "{w}x{h}"
            );
        }
    }

    #[test]
    fn downscale_averages_area() {
        // 左半黑右半白，缩成 1×1 应是中灰。
        let mut img = solid(4, 2, [0, 0, 0]);
        for y in 0..2 {
            for x in 2..4 {
                img.px[(y * 4 + x) * 4..][..3].copy_from_slice(&[255, 255, 255]);
            }
        }
        let p = img.resize(1, 1).at(0, 0);
        assert!((127..=128).contains(&p[0]), "{p:?}");
    }

    #[test]
    fn crop_square_takes_center() {
        let mut img = solid(6, 2, [0, 0, 0]);
        img.px[(2) * 4] = 9; // 第一行第 3 个像素，裁后应在 (0,0)
        let sq = img.crop_square();
        assert_eq!((sq.w, sq.h), (2, 2));
        assert_eq!(sq.at(0, 0)[0], 9);
    }

    #[test]
    fn round_corners_clears_corners_keeps_center() {
        let mut img = solid(20, 20, [255, 255, 255]);
        img.round_corners(6.0);
        assert_eq!(img.at(0, 0)[3], 0);
        assert_eq!(img.at(19, 19)[3], 0);
        assert_eq!(img.at(10, 10)[3], 255);
        assert_eq!(img.at(10, 0)[3], 255, "直边不受影响");
        // 预乘：颜色随 alpha 一起降。
        assert!(img.px.as_chunks::<4>().0.iter().all(|p| p[0] <= p[3]));
    }

    #[test]
    fn blend_over_and_clip() {
        let mut dst = solid(4, 4, [0, 0, 0]);
        let src = solid(2, 2, [255, 255, 255]);
        dst.blend(&src, 3, 3, 255); // 只有一个像素落在画布内
        assert_eq!(dst.at(3, 3), [255, 255, 255, 255]);
        assert_eq!(dst.at(2, 2), [0, 0, 0, 255]);
        let mut half = solid(1, 1, [0, 0, 0]);
        half.blend(&solid(1, 1, [255, 255, 255]), 0, 0, 128);
        assert_eq!(half.at(0, 0)[0], 128);
        // 完全越界不 panic。
        dst.blend(&src, -10, 50, 255);
    }

    #[test]
    fn glow_is_opaque_and_centered() {
        let mut img = Image::new(10, 10);
        img.glow([200, 0, 0], [0, 0, 0], 0.0, 0.0, 10.0);
        assert!(img.px.as_chunks::<4>().0.iter().all(|p| p[3] == 255));
        assert!(img.at(0, 0)[2] > img.at(5, 5)[2]);
        assert_eq!(img.at(9, 9)[2], 0, "半径外就是底色");
    }

    #[test]
    fn pick_colors_ignores_grey_and_finds_dominant() {
        assert_eq!(
            pick_colors(&solid(32, 32, [128, 128, 128])),
            None,
            "纯灰不投票"
        );
        assert_eq!(pick_colors(&solid(32, 32, [5, 5, 5])), None, "太暗不投票");

        let mut img = solid(32, 32, [30, 30, 30]);
        // 大半是红，一小块蓝。
        for y in 0..32 {
            for x in 0..24 {
                img.px[(y * 32 + x) * 4..][..3].copy_from_slice(&[20, 20, 220]);
            }
        }
        for y in 0..8 {
            for x in 24..32 {
                img.px[(y * 32 + x) * 4..][..3].copy_from_slice(&[220, 20, 20]);
            }
        }
        let (a, b) = pick_colors(&img).unwrap();
        assert!(a[0] > a[2], "主色应是红：{a:?}");
        assert!(b[0] < a[0] && b[0] < 60, "底色要压暗：{b:?}");
    }

    #[test]
    fn disc_is_round() {
        let d = disc(10, [255, 255, 255]);
        assert_eq!(d.at(0, 0)[3], 0);
        assert_eq!(d.at(5, 5)[3], 255);
    }
}
