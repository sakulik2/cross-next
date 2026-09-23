// 生成 assets/tray.ico —— 托盘图标（蓝底圆角方块 + 白色八分音符）。
//
// 为什么用脚本生成而不是直接提交一个画好的 .ico：仓库里的二进制 blob 没人能 review，
// 改一版颜色也无从下手。这个脚本只用 Node 标准库（和 tests/render.mjs 一个路子），
// 跑一次就能复现出字节完全相同的文件：
//
//     node tools/make-icon.mjs
//
// 它同时在终端打一份 ASCII 预览，改完形状不必真去看图就能判断还认不认得出来。
//
// 图标不进构建流程 —— tray.rs 用 include_bytes! 把它编进 exe，所以改完这个脚本
// 要重新跑一次并提交 assets/tray.ico，否则 exe 里还是旧图。

import { writeFileSync, mkdirSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";

/// 两档尺寸：16 给托盘（SM_CXSMICON 在 100% 缩放下就是 16），32 给高 DPI 与
/// Alt-Tab / 任务管理器。tray.rs 会按实际的小图标尺寸挑最接近的一张。
const SIZES = [16, 32];

/// 每像素 4x4 超采样。图形是解析式定义的，靠超采样拿抗锯齿，
/// 不必自己写边缘处理。
const SS = 4;

// 渐变端点（sRGB）。左上浅、右下深，托盘里在浅色和深色任务栏上都还分得出来。
const C0 = [0x7a, 0xb8, 0xff];
const C1 = [0x2a, 0x54, 0xe0];

// ---- 形状：全部用 [0,1] 归一化坐标，y 向下 ----

/// 圆角方块。留 3% 边距，免得贴着画布边被切。
function inPlate(x, y) {
  const m = 0.03;
  const r = 0.22;
  const dx = Math.max(m + r - x, x - (1 - m - r), 0);
  const dy = Math.max(m + r - y, y - (1 - m - r), 0);
  return dx > 0 && dy > 0 ? dx * dx + dy * dy <= r * r : true;
}

/// 符头：压扁并左倾的椭圆，符合音符的惯常画法。
function inHead(x, y) {
  const cx = 0.42;
  const cy = 0.685;
  const rx = 0.175;
  const ry = 0.127;
  const a = (-22 * Math.PI) / 180;
  const dx = x - cx;
  const dy = y - cy;
  const u = dx * Math.cos(a) + dy * Math.sin(a);
  const v = -dx * Math.sin(a) + dy * Math.cos(a);
  return (u * u) / (rx * rx) + (v * v) / (ry * ry) <= 1;
}

/// 符干。底端插进符头内部，两者并起来才是一体的。
function inStem(x, y) {
  return x >= 0.545 && x <= 0.607 && y >= 0.25 && y <= 0.70;
}

/// 符尾：两条曲线夹出的月牙。两端收尖（t=0 在符干顶、t=1 在右下），中间最宽。
function inFlag(x, y) {
  const yTop = 0.25;
  const yBot = 0.48;
  if (y < yTop || y > yBot) return false;
  const t = (y - yTop) / (yBot - yTop);
  const outer = 0.6 + 0.215 * Math.sin(t * 0.9 * (Math.PI / 2));
  const inner = 0.6 + 0.215 * Math.pow(t, 2.2);
  return x >= Math.min(outer, inner) && x <= Math.max(outer, inner);
}

const inNote = (x, y) => inHead(x, y) || inStem(x, y) || inFlag(x, y);

/// 渲染一张 size×size 的 BGRA 位图（自上而下）。
function render(size) {
  const px = new Uint8Array(size * size * 4);
  const step = 1 / (size * SS);

  for (let row = 0; row < size; row++) {
    for (let col = 0; col < size; col++) {
      let plate = 0;
      let note = 0;

      // 超采样：数覆盖到的子样本个数，得到 0..1 的覆盖率。
      for (let sy = 0; sy < SS; sy++) {
        for (let sx = 0; sx < SS; sx++) {
          const x = (col * SS + sx + 0.5) * step;
          const y = (row * SS + sy + 0.5) * step;
          if (inPlate(x, y)) plate++;
          if (inNote(x, y)) note++;
        }
      }

      const total = SS * SS;
      plate /= total;
      note /= total;

      const i = (row * size + col) * 4;
      if (plate === 0) continue; // 保持全透明

      // 底色取对角渐变。
      const t = (col / (size - 1) + row / (size - 1)) / 2;
      const base = [0, 1, 2].map((k) => C0[k] + (C1[k] - C0[k]) * t);

      // 音符压在底色上；音符只落在圆角方块内部，所以直接按覆盖率混白。
      const rgb = base.map((c) => c + (255 - c) * note);

      // BGRA，直通 alpha（ICO 的约定，不是预乘）。
      px[i] = Math.round(rgb[2]);
      px[i + 1] = Math.round(rgb[1]);
      px[i + 2] = Math.round(rgb[0]);
      px[i + 3] = Math.round(plate * 255);
    }
  }

  return px;
}

/// 打包成一张 ICONIMAGE：BITMAPINFOHEADER + XOR 位图 + AND 掩码。
function iconImage(size, px) {
  const header = Buffer.alloc(40);
  header.writeUInt32LE(40, 0); // biSize
  header.writeInt32LE(size, 4); // biWidth
  // biHeight 是 XOR 与 AND 两块加起来的高度，所以要乘 2 —— 这是 ICO 的老约定，
  // 写成 size 会让系统把图读成上半截。
  header.writeInt32LE(size * 2, 8);
  header.writeUInt16LE(1, 12); // biPlanes
  header.writeUInt16LE(32, 14); // biBitCount
  header.writeUInt32LE(0, 16); // biCompression = BI_RGB

  // XOR 位图自下而上存。
  const xor = Buffer.alloc(size * size * 4);
  for (let row = 0; row < size; row++) {
    const src = (size - 1 - row) * size * 4;
    Buffer.from(px.buffer, src, size * 4).copy(xor, row * size * 4);
  }

  // 32bpp 的透明由 alpha 决定，但 AND 掩码这一块必须在 —— 少了它有些加载路径
  // 会按残缺数据处理。全 0 表示"全部不透明"，正好把决定权交给 alpha。
  // 每行按 4 字节对齐。
  const maskStride = Math.ceil(size / 32) * 4;
  const mask = Buffer.alloc(maskStride * size);

  return Buffer.concat([header, xor, mask]);
}

/// 组装 ICO 文件：ICONDIR + 每张图一条 ICONDIRENTRY + 图像数据。
function buildIco(images) {
  const dir = Buffer.alloc(6 + images.length * 16);
  dir.writeUInt16LE(0, 0); // 保留
  dir.writeUInt16LE(1, 2); // 类型 1 = 图标
  dir.writeUInt16LE(images.length, 4);

  let offset = dir.length;
  images.forEach(({ size, data }, i) => {
    const e = 6 + i * 16;
    // 宽高各占一个字节，256 要写 0 —— 这里最大 32，用不上那条特例。
    dir.writeUInt8(size, e);
    dir.writeUInt8(size, e + 1);
    dir.writeUInt8(0, e + 2); // 调色板颜色数：真彩色为 0
    dir.writeUInt8(0, e + 3); // 保留
    dir.writeUInt16LE(1, e + 4); // 平面数
    dir.writeUInt16LE(32, e + 6); // 位深
    dir.writeUInt32LE(data.length, e + 8);
    dir.writeUInt32LE(offset, e + 12);
    offset += data.length;
  });

  return Buffer.concat([dir, ...images.map((i) => i.data)]);
}

/// ASCII 预览。按亮度取字符，透明处留空 —— 形状认不出来时改脚本比翻图快。
function preview(size, px) {
  const ramp = " .:-=+*#%@";
  const lines = [];
  for (let row = 0; row < size; row++) {
    let line = "";
    for (let col = 0; col < size; col++) {
      const i = (row * size + col) * 4;
      const a = px[i + 3];
      if (a < 32) {
        line += "  ";
        continue;
      }
      const lum = (px[i] * 0.11 + px[i + 1] * 0.59 + px[i + 2] * 0.3) / 255;
      const ch = ramp[Math.min(ramp.length - 1, Math.round(lum * (ramp.length - 1)))];
      line += ch + ch; // 字符宽高比约 1:2，横向重复一遍才不至于压扁
    }
    lines.push(line);
  }
  return lines.join("\n");
}

const root = join(dirname(fileURLToPath(import.meta.url)), "..");
const out = join(root, "assets", "tray.ico");

const images = SIZES.map((size) => {
  const px = render(size);
  console.log(`\n${size}x${size}:`);
  console.log(preview(size, px));
  return { size, data: iconImage(size, px) };
});

mkdirSync(dirname(out), { recursive: true });
writeFileSync(out, buildIco(images));
console.log(`\n已写入 ${out}`);
