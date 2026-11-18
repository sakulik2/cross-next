// 生成 assets/ 下的图标：
//
//   tray.ico                           程序图标（蓝底圆角方块 + 白色八分音符），
//                                      托盘与 desktop.exe 的窗口/任务栏图标共用
//   prev.ico play.ico pause.ico next.ico
//                                      desktop.exe 的按钮字形：白色、透明底，
//                                      小组件与任务栏缩略图按钮共用
//
// 为什么用脚本生成而不是直接提交一个画好的 .ico：仓库里的二进制 blob 没人能 review，
// 改一版颜色也无从下手。这个脚本只用 Node 标准库（和 tests/render.mjs 一个路子），
// 跑一次就能复现出字节完全相同的文件：
//
//     node tools/make-icon.mjs
//
// 它同时在终端打一份 ASCII 预览，改完形状不必真去看图就能判断还认不认得出来。
//
// 图标不进构建流程 —— 各处用 include_bytes! 把它们编进 exe，所以改完这个脚本
// 要重新跑一次并提交 assets/ 下的产物，否则 exe 里还是旧图。

import { writeFileSync, mkdirSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";

/// 程序图标的尺寸档：小图标（托盘、标题）在 100%/125%/150%/200% 缩放下是
/// 16/20/24/32，任务栏与 Alt-Tab 要到 48。64-128 给 desktop.exe 画大图用
/// （小组件的占位图标在 200% 缩放下是 72 像素）—— 从 48 放大上去边缘全是锯齿，
/// 所以那边一律挑不小于目标的一张往下缩，这几档就是为它备的。
const SIZES = [16, 20, 24, 32, 48, 64, 96, 128];

/// 按钮字形的尺寸档：缩略图按钮用小图标尺寸（16-32），小组件上画 20-24 DIP，
/// 到 200% 缩放是 40-48，250% 是 50-60，由 64 缩下来。
const GLYPH_SIZES = [16, 20, 24, 32, 40, 48, 64];

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
  // 边距对直边同样生效。之前只有圆角往里缩了，四条直边却顶到画布边缘，
  // 圆弧与直边接不上，每个角都有一个台阶 —— 16/32 像素下看不出，放大到
  // desktop.exe 的 72 像素才露出来。
  if (x < m || x > 1 - m || y < m || y > 1 - m) return false;
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

// ---- 按钮字形：同样是 [0,1] 归一化坐标 ----

/// 点是否在三角形内（顶点顺序无关）。
function inTri(x, y, [ax, ay], [bx, by], [cx, cy]) {
  const d1 = (x - bx) * (ay - by) - (ax - bx) * (y - by);
  const d2 = (x - cx) * (by - cy) - (bx - cx) * (y - cy);
  const d3 = (x - ax) * (cy - ay) - (cx - ax) * (y - ay);
  const neg = d1 < 0 || d2 < 0 || d3 < 0;
  const pos = d1 > 0 || d2 > 0 || d3 > 0;
  return !(neg && pos);
}

const inRect = (x, y, x0, y0, x1, y1) => x >= x0 && x <= x1 && y >= y0 && y <= y1;

/// 播放三角的几何重心在左边三分之一处，看着偏左，所以整体右移一点做视觉居中。
const GLYPHS = {
  play: (x, y) => inTri(x, y, [0.3, 0.17], [0.3, 0.83], [0.86, 0.5]),
  pause: (x, y) => inRect(x, y, 0.25, 0.19, 0.42, 0.81) || inRect(x, y, 0.58, 0.19, 0.75, 0.81),
  next: (x, y) => inTri(x, y, [0.18, 0.2], [0.18, 0.8], [0.6, 0.5]) || inRect(x, y, 0.68, 0.2, 0.8, 0.8),
  prev: (x, y) => inTri(x, y, [0.82, 0.2], [0.82, 0.8], [0.4, 0.5]) || inRect(x, y, 0.2, 0.2, 0.32, 0.8),
};

/// 白色字形，alpha 即覆盖率。直通 alpha，同上。
function renderGlyph(size, inside) {
  const px = new Uint8Array(size * size * 4);
  const step = 1 / (size * SS);
  for (let row = 0; row < size; row++) {
    for (let col = 0; col < size; col++) {
      let hit = 0;
      for (let sy = 0; sy < SS; sy++) {
        for (let sx = 0; sx < SS; sx++) {
          if (inside((col * SS + sx + 0.5) * step, (row * SS + sy + 0.5) * step)) hit++;
        }
      }
      const i = (row * size + col) * 4;
      px[i] = px[i + 1] = px[i + 2] = 255;
      px[i + 3] = Math.round((hit / (SS * SS)) * 255);
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
    // 宽高各占一个字节，256 要写 0 —— 这里最大 128，用不上那条特例。
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
mkdirSync(join(root, "assets"), { recursive: true });

/// 写一个 .ico，并打印 `show` 那一档的预览。
function emit(name, sizes, draw, show) {
  const images = sizes.map((size) => {
    const px = draw(size);
    if (show.includes(size)) {
      console.log(`
${name} ${size}x${size}:`);
      console.log(preview(size, px));
    }
    return { size, data: iconImage(size, px) };
  });
  const out = join(root, "assets", name);
  writeFileSync(out, buildIco(images));
  console.log(`已写入 ${out}`);
}

emit("tray.ico", SIZES, render, [16, 32]);
for (const [name, inside] of Object.entries(GLYPHS)) {
  emit(`${name}.ico`, GLYPH_SIZES, (size) => renderGlyph(size, inside), [20]);
}
