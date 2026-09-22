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
// 监听器要记下来而不是丢掉：进度条冻结那个 bug 出在事件处理里，不在 render() 里，
// 只调 render() 测不到它。记下来测试才能真的派发一次 pointerdown/pointerup。
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
  _on: {},
  addEventListener(type, fn) {
    (this._on[type] ??= []).push(fn);
  },
  fire(type, ev = {}) {
    (this._on[type] || []).forEach((fn) => fn.call(this, ev));
  },
});

const nodes = {};
// 暴露给 eval 里的断言用 —— 有些用例要检查元素的最终状态，不只是「没抛异常」。
globalThis.__nodes = nodes;
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
// window 级监听器同样要记下来 —— pointerup 的解除逻辑挂在这里。
const winOn = {};
globalThis.addEventListener = (type, fn) => {
  (winOn[type] ??= []).push(fn);
};
globalThis.__fireWindow = (type, ev = {}) => {
  (winOn[type] || []).forEach((fn) => fn(ev));
};
globalThis.setInterval = () => 0;
// 进度条插值要用。now 可推进，这样才能验证播放中的外推。
let fakeNow = 1000;
globalThis.performance = { now: () => fakeNow };
globalThis.__advance = (ms) => { fakeNow += ms; };
globalThis.requestAnimationFrame = () => 0;
// pointerup 的解除是延迟一拍做的，测试要能把它推进完。
globalThis.__timers = [];
const realSetTimeout = globalThis.setTimeout;
globalThis.setTimeout = (fn, ms) => {
  // 0ms 的留给 Image.onload 之类的真异步，其余记下来由测试显式跑。
  if (ms) { globalThis.__timers.push(fn); return 0; }
  return realSetTimeout(fn, ms);
};
globalThis.__runTimers = () => {
  const ts = globalThis.__timers;
  globalThis.__timers = [];
  ts.forEach((fn) => fn());
};

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

// canSeek=false 仍要能拖。QQ音乐 上报的能力位不可信（报 false 却真的能跳），
// 照它禁用滑杆等于白废一个能用的功能。这条守着别人「照能力位禁用」改回去。
try {
  render({ present: true, mode: "smtc", matched: true, playing: true,
           title: "歌", artTag: "s1", position: 30, duration: 200, canSeek: false });
  const bar = globalThis.__nodes["seekBar"];
  if (bar.disabled) {
    console.log("  FAIL  canSeek=false 时滑杆被禁用了 —— 能力位不可信，不该照它禁用");
    failed++;
  } else {
    console.log("  ok    canSeek=false 仍可拖动");
  }
} catch (e) {
  console.log("  FAIL  canSeek=false 仍可拖动: " + e.message);
  failed++;
}

// 原地点一下进度条（pointerdown 后没有 change）不该让进度条永久冻结。
//
// 这是实际发生过的 bug：seekDragging 只在 change 里清，而值没变时 change 不触发，
// 于是标志永久卡在 true，paint() 从此直接返回。表现是 elapsed 冻在按下那一刻、
// total 仍随轮询更新，换歌后就成了 3:41/2:01 —— 前半截还是上一首的进度。
//
// 必须真的派发事件：bug 在事件处理里，只调 render() 测不到。
try {
  const bar = globalThis.__nodes["seekBar"];
  const elapsed = globalThis.__nodes["elapsed"];

  render({ present: true, mode: "smtc", matched: true, playing: true,
           title: "长的", artTag: "t-long", position: 221, duration: 332, canSeek: false });
  const frozen = elapsed.textContent;

  // 原地按下再松开，不改变 value，所以不会有 change 事件。
  bar.fire("pointerdown");
  globalThis.__fireWindow("pointerup");
  globalThis.__runTimers(); // 跑掉解除标志的那一拍

  // 换一首短歌。若标志还卡着，elapsed 会留在上一首的 3:41。
  render({ present: true, mode: "smtc", matched: true, playing: true,
           title: "短的", artTag: "t-short", position: 3, duration: 121, canSeek: false });

  if (elapsed.textContent === frozen) {
    console.log("  FAIL  原地点击后进度条冻结：换歌了 elapsed 仍是 " + frozen);
    failed++;
  } else {
    console.log("  ok    原地点击不会冻结进度条");
  }
} catch (e) {
  console.log("  FAIL  原地点击不会冻结进度条: " + e.constructor.name + ": " + e.message);
  failed++;
}

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
