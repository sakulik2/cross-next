// 假的 cross-next 服务端，给 desktop.exe 的实机调试用：
//
//     node tools/fake-server.mjs [端口，默认 18771]
//
// 然后把 desktop.exe 旁边 remote.json 的 host/port 指向 127.0.0.1 和这个端口
// （token 随便填，这里不校验）。desktop.exe 会热重载 remote.json，不必重启。
//
// 为什么需要它：没有 QQ音乐 时真服务端处在 none 模式，按下任何切歌键都会退化成
// **全局媒体键** —— 若这台机器上还开着 listen.exe，它会把键转发到真正的服务端，
// 测个按钮就把另一台机器上的音乐切了。这里两首假曲目、两张生成的封面、一个会走的
// 进度，命令只改内存里的状态并打印出来，按键有了安全的落点。
//
// 第一首曲名带引号，用来验转义；第二首曲名很长、没有专辑，用来验省略与单行。
import http from "node:http";
import zlib from "node:zlib";

const PORT = Number(process.argv[2] || 18771);

// ---- 极简 PNG 编码 ----
const crcTable = Array.from({ length: 256 }, (_, n) => {
  let c = n;
  for (let k = 0; k < 8; k++) c = c & 1 ? 0xedb88320 ^ (c >>> 1) : c >>> 1;
  return c >>> 0;
});
const crc32 = (buf) => {
  let c = 0xffffffff;
  for (const b of buf) c = crcTable[(c ^ b) & 0xff] ^ (c >>> 8);
  return (c ^ 0xffffffff) >>> 0;
};
function chunk(type, data) {
  const len = Buffer.alloc(4);
  len.writeUInt32BE(data.length);
  const td = Buffer.concat([Buffer.from(type), data]);
  const crc = Buffer.alloc(4);
  crc.writeUInt32BE(crc32(td));
  return Buffer.concat([len, td, crc]);
}
function png(w, h, pixel) {
  const raw = Buffer.alloc((w * 3 + 1) * h);
  for (let y = 0; y < h; y++) {
    raw[y * (w * 3 + 1)] = 0;
    for (let x = 0; x < w; x++) {
      const [r, g, b] = pixel(x / w, y / h);
      const i = y * (w * 3 + 1) + 1 + x * 3;
      raw[i] = r; raw[i + 1] = g; raw[i + 2] = b;
    }
  }
  const ihdr = Buffer.alloc(13);
  ihdr.writeUInt32BE(w, 0); ihdr.writeUInt32BE(h, 4);
  ihdr[8] = 8; ihdr[9] = 2;
  return Buffer.concat([
    Buffer.from([0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a]),
    chunk("IHDR", ihdr), chunk("IDAT", zlib.deflateSync(raw)), chunk("IEND", Buffer.alloc(0)),
  ]);
}
const mix = (a, b, t) => a.map((v, i) => Math.round(v + (b[i] - v) * t));
function cover(c0, c1) {
  return png(400, 400, (x, y) => {
    const d = Math.hypot(x - 0.5, y - 0.45);
    if (d < 0.18) return [245, 240, 230];
    if (Math.abs(d - 0.3) < 0.015) return [250, 250, 250];
    return mix(c0, c1, (x + y) / 2);
  });
}

const TRACKS = [
  { title: "\"Heroes\" 示例曲目", artist: "测试歌手", album: "假专辑 A", art: cover([230, 90, 40], [120, 20, 60]), duration: 240 },
  { title: "第二首：很长很长很长很长很长很长的曲名用来测试省略号", artist: "另一位歌手", album: "", art: cover([30, 170, 160], [20, 50, 110]), duration: 180 },
];
let idx = 0, playing = true, pos = 30, volume = 0.5, muted = false, last = Date.now();
const tick = () => { const now = Date.now(); if (playing) pos += (now - last) / 1000; last = now; };

http.createServer((req, res) => {
  let body = "";
  req.on("data", (c) => (body += c));
  req.on("end", () => {
    tick();
    const url = new URL(req.url, "http://x");
    // 客户端按 Content-Length 精确读，不带的话 Node 会走 chunked，客户端读到空体。
    const json = (o) => { const b = Buffer.from(JSON.stringify(o)); res.writeHead(200, { "Content-Type": "application/json", "Content-Length": b.length, Connection: "close" }); res.end(b); };
    if (req.method !== "GET" || url.pathname !== "/api/state") console.log(new Date().toISOString().slice(11, 23), req.method, url.pathname, body);
    if (url.pathname === "/api/state") {
      const t = TRACKS[idx];
      return json({ present: true, mode: "smtc", matched: true, playing, aumid: "QQMusic", title: t.title, artist: t.artist, album: t.album,
        artTag: "tag" + idx, position: pos, duration: t.duration, canSeek: false, volume, muted });
    }
    if (url.pathname === "/api/cmd") {
      const a = JSON.parse(body).action;
      if (a === "playpause") playing = !playing;
      if (a === "next" || a === "prev") { idx = (idx + 1) % TRACKS.length; pos = 0; }
      return json({ accepted: true });
    }
    if (url.pathname === "/api/volume") {
      const o = JSON.parse(body);
      if ("level" in o) volume = o.level;
      if ("mute" in o) muted = o.mute;
      return json({ accepted: true });
    }
    if (url.pathname === "/api/thumbnail") {
      const b = TRACKS[idx].art;
      res.writeHead(200, { "Content-Type": "image/png", "Content-Length": b.length, Connection: "close" });
      return res.end(b);
    }
    res.writeHead(404, { Connection: "close", "Content-Length": 0 }); res.end();
  });
}).listen(PORT, "127.0.0.1", () => console.log("fake server on", PORT));
