// 前端渲染测试。零依赖，`node tests/render.mjs` 直接跑。
//
// 抓的是运行时错误 —— 尤其是 ReferenceError。这类 bug 静态检查看不出来，而且很致命：
// render() 中途抛异常会让它后面的全部更新静默失效（曾经因此同时坏掉图标和封面）。
//
// 做法是把页面的 <script> 抽出来，在 stub DOM 下 eval，然后拿各种状态调 render()。

import { readFileSync } from "node:fs";
import { fileURLToPath } from "node:url";
import { dirname, join } from "node:path";

const root = join(dirname(fileURLToPath(import.meta.url)), "..");

// ---- 最小 DOM stub ----
// 页面每用一个新的 DOM API，这里就得跟着补。漏了会得到假失败 ——
// 开发这套测试时就分别撞上过 document.documentElement 和元素的 addEventListener。
const el = () => ({
  textContent: "",
  hidden: false,
  value: 0,
  disabled: false,
  onclick: null,
  classList: { add() {}, remove() {}, toggle() {}, contains: () => false },
  setAttribute() {},
  getAttribute: () => null,
  style: { setProperty() {} },
  addEventListener() {},
});

const nodes = {};
globalThis.document = {
  getElementById: (id) => (nodes[id] ??= el()),
  querySelector: () => el(),
  body: el(),
  documentElement: el(),
  hidden: false,
  createElement: () => ({
    width: 0,
    height: 0,
    getContext: () => ({
      drawImage() {},
      // 全零像素，pickColors 应走「无主色」分支
      getImageData: () => ({ data: new Uint8Array(16 * 16 * 4) }),
    }),
  }),
};
globalThis.localStorage = { getItem: () => "tok", setItem() {} };
globalThis.location = { href: "http://x.test/", pathname: "/", hash: "" };
globalThis.history = { replaceState() {} };
globalThis.URL = class {
  constructor() {
    this.searchParams = { get: () => null, delete() {} };
    this.pathname = "/";
    this.hash = "";
  }
  static createObjectURL() {
    return "blob:x";
  }
};
globalThis.Image = class {
  set src(v) {
    this._s = v;
    setTimeout(() => this.onload?.(), 0);
  }
  get src() {
    return this._s;
  }
};
globalThis.fetch = async () => ({
  ok: true,
  status: 200,
  headers: { get: () => '"etag"' },
  blob: async () => ({ size: 10 }),
  json: async () => ({}),
});
globalThis.addEventListener = () => {};
globalThis.setInterval = () => 0;
// 进度条插值要用
globalThis.performance = { now: () => 1000 };
globalThis.requestAnimationFrame = () => 0;

// ---- 抽出页面脚本 ----
const html = readFileSync(join(root, "web", "index.html"), "utf8");
// 不匹配行尾换行：git 的 autocrlf 在 Windows 上会把文件检出成 CRLF，
// 写死 \n 会匹配不上。
const match = html.match(/<script>([\s\S]*?)<\/script>/);
if (!match) {
  console.error("在 web/index.html 里找不到 <script> 块");
  process.exit(1);
}
// 去掉启动时的轮询，否则测试一加载就开始打接口。
// `\r?` 同样是为了兼容 autocrlf 检出的 CRLF。
const page = match[1]
  .replace(/^poll\(\);\r?$/m, "")
  .replace(/^setInterval\(poll, 1000\);\r?$/m, "");

// ---- 用例 ----
// 覆盖 render() 的每个分支。字段形状对齐 /api/state 的真实输出。
const cases = [
  ["无会话", { present: false }],
  ["mediakey 有音量", { present: true, mode: "mediakey", volume: 0.5, muted: false }],
  ["mediakey 无音量", { present: true, mode: "mediakey", volume: null, muted: null }],
  ["smtc 有进度", {
    present: true, mode: "smtc", matched: true, playing: true,
    title: "歌", artist: "手", album: "辑", artTag: "a",
    volume: 0.7, muted: false, position: 45.2, duration: 210.5, canSeek: true,
  }],
  ["smtc 无时间轴", {
    present: true, mode: "smtc", matched: true, playing: true, title: "歌", artTag: "b",
    volume: 0.7, muted: false, position: null, duration: null, canSeek: false,
  }],
  // 直播流会报这个：EndTime <= StartTime，服务端降级为 duration 0/null
  ["duration 为 0", {
    present: true, mode: "smtc", matched: true, playing: true, title: "歌", artTag: "c",
    position: 0, duration: 0, canSeek: false,
  }],
  // 会话可以既有时间轴又不允许 seek
  ["不可 seek 有进度", {
    present: true, mode: "smtc", matched: true, playing: false, title: "歌", artTag: "d",
    position: 10, duration: 100, canSeek: false,
  }],
  ["暂停中", {
    present: true, mode: "smtc", matched: true, playing: false, title: "歌", artTag: "e",
    position: 99.9, duration: 100, canSeek: true,
  }],
  ["未锁定 target", {
    present: true, mode: "smtc", matched: false, aumid: "Other.exe", playing: true,
    title: "x", artTag: "f", volume: 0.3, muted: true,
  }],
  ["服务端报错", { error: "SMTC 初始化失败" }],
  // 缺字段不该抛异常 —— 服务端出错时可能只回一部分
  ["缺字段容错", { present: true, mode: "smtc", matched: true }],
];

// 测试代码必须追加到同一个 eval 字符串里：页面是 "use strict"，严格模式下
// eval 内的函数声明只在该 eval 作用域可见，分开写会得到 render is not defined。
const suite = `
let failed = 0;
const cases = ${JSON.stringify(cases)};

for (const [name, state] of cases) {
  try {
    render(state);
    console.log("  ok    " + name);
  } catch (e) {
    console.log("  FAIL  " + name + ": " + e.constructor.name + ": " + e.message);
    failed++;
  }
}

// fmtTime 的边界
const times = [[0,"0:00"], [5,"0:05"], [65,"1:05"], [3599,"59:59"], [-1,"0:00"], [NaN,"0:00"]];
let timeBad = 0;
for (const [input, want] of times) {
  const got = fmtTime(input);
  if (got !== want) {
    console.log("  FAIL  fmtTime(" + input + ") = " + got + "，期望 " + want);
    timeBad++;
  }
}
if (timeBad === 0) console.log("  ok    fmtTime 六个边界");
failed += timeBad;

// 模式来回切换：状态不该残留（封面 etag、进度条可见性等）
try {
  render({ present: true, mode: "mediakey", volume: 0.4, muted: false });
  render({ present: true, mode: "smtc", matched: true, playing: true,
           title: "a", artTag: "t1", position: 5, duration: 100, canSeek: true });
  render({ present: false });
  render({ present: true, mode: "smtc", matched: true, playing: false,
           title: "b", artTag: "t2", position: null, duration: null, canSeek: false });
  console.log("  ok    模式来回切换");
} catch (e) {
  console.log("  FAIL  模式来回切换: " + e.message);
  failed++;
}

globalThis.__failed = failed;
`;

eval(page + suite);

const failed = globalThis.__failed;
console.log(failed ? `\n${failed} 个失败` : "\n全部通过");
process.exit(failed ? 1 : 0);
