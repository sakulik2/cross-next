//! 手写的最小 HTTP/1.1 服务端。
//!
//! 只需要伺候一个自家前端：GET 几个 JSON、POST 几个命令、吐一张封面图。为此引入
//! axum 会连带 tokio 一整套，与本项目零依赖的取向不符，也没有对应的收益。
//!
//! 每个连接一个线程。并发量是"家里两三台设备"这个量级，线程池都算过度设计。

use crate::media::{Command, Remote, Reply, Snapshot};
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{Ipv4Addr, SocketAddr, TcpListener, TcpStream};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

/// 内嵌前端。单文件，改完重新编译即可，不必操心运行时的相对路径。
const INDEX_HTML: &str = include_str!("../web/index.html");

/// 请求体上限。命令类请求都只有几十字节，给足余量即可。
const MAX_BODY: usize = 4 * 1024;
/// 请求头上限，防着慢速攻击和畸形请求占着线程。
const MAX_HEADER_BYTES: usize = 16 * 1024;
/// 单行上限（请求行与每条头各自适用）。
///
/// 必须**边读边限**而不是读完再查：`read_line` 会一直读到 `\n` 为止，中途不断
/// 增长那个 String。只在读完后比较累计字节数的话，一个持续发送但永不发 `\n` 的
/// 连接就能把内存耗尽 —— 而且这发生在 token 校验之前，同网段任何设备都能做到。
const MAX_LINE: usize = 8 * 1024;
/// 读写超时。浏览器的 keep-alive 连接闲置后由超时收掉。
const IO_TIMEOUT: Duration = Duration::from_secs(30);
/// 连上之后必须在这段时间内把**第一行**发出来。
///
/// 与 `IO_TIMEOUT` 分开，是因为两者防的事不同：`IO_TIMEOUT` 要容得下前端的
/// keep-alive 连接在两次轮询之间闲着（正常间隔 1 秒，给到 30 秒很宽松），而刚连上
/// 就一言不发的连接没有任何正当理由 —— 它只是在占着名额。实测 200 个这样的空连接
/// 能把 `MAX_CONNECTIONS` 占满，让正常请求被拒；把首行的耐心收紧到 5 秒，
/// 名额就会快速周转。
///
/// 只作用于每个请求的第一行：一旦开始发，后续读取回到 `IO_TIMEOUT`。
const HEADER_TIMEOUT: Duration = Duration::from_secs(5);
/// 同时在处理的连接数上限。
///
/// 每连接一个线程，加上读超时，没有上限的话同网段任何设备开一批不说话的连接就能
/// 占满线程 —— 而 `thread::spawn` 建不出线程时是 panic，配上 `panic = "abort"`
/// 等于整个进程被弄没。这个量级远超"家里两三台设备"的正常用量，正常使用碰不到。
const MAX_CONNECTIONS: usize = 64;

pub struct Server {
    pub listener: TcpListener,
    pub token: String,
    pub media: Remote,
}

/// 在处理中的连接计数。析构即归还，所以线程怎么退出都不会漏计。
struct ConnGuard(Arc<AtomicUsize>);

impl Drop for ConnGuard {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::Relaxed);
    }
}

impl Server {
    /// 接受连接并逐个处理，永不返回。
    pub fn serve(self) {
        let active = Arc::new(AtomicUsize::new(0));

        for stream in self.listener.incoming() {
            let Ok(stream) = stream else { continue };

            // 超过上限直接断开：stream 在这一轮结束时析构。
            if active.fetch_add(1, Ordering::Relaxed) >= MAX_CONNECTIONS {
                active.fetch_sub(1, Ordering::Relaxed);
                continue;
            }
            let guard = ConnGuard(Arc::clone(&active));

            let token = self.token.clone();
            let media = self.media.clone();

            // 用 Builder::spawn 而不是 thread::spawn：后者在系统建不出线程时是
            // **panic**，而 release 配的是 panic = "abort" —— 等于让对端能靠灌连接
            // 把整个进程弄没。这里失败就丢掉这个连接继续 accept。
            //
            // 失败时闭包随之析构，guard 把计数还回去，不必手动处理。
            let _ = std::thread::Builder::new()
                .name("http".into())
                .spawn(move || {
                    // guard 移进线程，随线程结束归还计数。
                    let _guard = guard;
                    let _ = stream.set_read_timeout(Some(IO_TIMEOUT));
                    let _ = stream.set_write_timeout(Some(IO_TIMEOUT));
                    // 单个连接出错不该影响整个服务，所以忽略错误。
                    handle(stream, &token, &media);
                });
        }
    }
}

struct Request {
    method: String,
    path: String,
    query: String,
    auth: Option<String>,
    /// `Host` 头。缺失是允许的 —— 浏览器一定会发，而且网页脚本改不了它，
    /// 所以"没有 Host"永远不是从浏览器页面发起的请求。
    host: Option<String>,
    body: Vec<u8>,
    /// 客户端请求的 `Connection: close`。命令行客户端（remote.exe）发一条就走，
    /// 不遵守这个头会让它一直等到读超时。
    close: bool,
}

fn handle(stream: TcpStream, token: &str, media: &Remote) {
    let mut reader = BufReader::new(match stream.try_clone() {
        Ok(s) => s,
        Err(_) => return,
    });
    let mut writer = stream;

    // 实际绑定的地址，用于校验 Host。取不到就跳过那项检查 ——
    // 宁可少一道纵深防御，也不要因此把正常请求拒掉。
    let bound = writer.local_addr().ok().and_then(|a| match a.ip() {
        std::net::IpAddr::V4(ip) => Some(ip),
        _ => None,
    });

    // keep-alive：同一连接上串行处理多个请求，前端每秒轮询时省掉握手。
    loop {
        // 等下一个请求的第一行时用短超时，占着名额一言不发的连接会被快速清掉。
        // read_request 读到请求行之后会自己把超时放回 IO_TIMEOUT。
        //
        // 必须设在 `reader` 的句柄上，不能设在 `writer` 上：`try_clone` 复制出的是
        // **另一个** socket 句柄，而 Windows 的 `SO_RCVTIMEO` 按句柄生效，不跟着
        // 复制走。设错句柄的话这行毫无作用，读取仍按 IO_TIMEOUT 等满 30 秒
        // （实测确认过）。
        let _ = reader.get_ref().set_read_timeout(Some(HEADER_TIMEOUT));

        let req = match read_request(&mut reader) {
            Ok(Some(r)) => r,
            // 连接正常关闭或请求畸形，都直接收摊。
            _ => return,
        };

        let resp = route(&req, token, media, bound);
        if write_response(&mut writer, &resp, req.close).is_err() {
            return;
        }
        // 客户端说了 close 就关掉 —— 它可能正在等对端关闭以判断响应结束。
        if req.close {
            return;
        }
    }
}

/// 读一行，长度设上限。
///
/// 关键在于**边读边限**而不是读完再查：`read_line` 会一直读到 `\n` 为止，中途不断
/// 增长那个 String。只在读完后比较累计字节数的话，一个持续发送但永不发 `\n` 的
/// 连接就能把内存耗尽 —— 而且这发生在 token 校验之前，同网段任何设备都能做到。
///
/// `Take` 让 `read_line` 最多读 `MAX_LINE` 字节就停。行尾没有 `\n` 即说明超长
/// （或 EOF 截断），两者都算畸形请求。
fn read_line_limited(
    reader: &mut BufReader<TcpStream>,
    line: &mut String,
) -> std::io::Result<usize> {
    reader.by_ref().take(MAX_LINE as u64).read_line(line)
}

fn read_request(reader: &mut BufReader<TcpStream>) -> std::io::Result<Option<Request>> {
    let mut line = String::new();
    // 读到 0 字节表示对端关闭了连接。
    if read_line_limited(reader, &mut line)? == 0 {
        return Ok(None);
    }
    if !line.ends_with('\n') {
        return Ok(None); // 请求行超长或被截断
    }

    // 对端确实在说话了，把超时放回正常值 —— 短超时只用于"连上却不发请求"。
    // 两个句柄指向同一个 socket，所以在这个 clone 上设置即可。
    let _ = reader.get_ref().set_read_timeout(Some(IO_TIMEOUT));

    let mut parts = line.split_whitespace();
    let (Some(method), Some(target)) = (parts.next(), parts.next()) else {
        return Ok(None);
    };
    let (path, query) = match target.split_once('?') {
        Some((p, q)) => (p.to_string(), q.to_string()),
        None => (target.to_string(), String::new()),
    };

    let mut auth = None;
    let mut host = None;
    let mut content_length = 0usize;
    let mut close = false;
    let mut header_bytes = line.len();

    loop {
        let mut header = String::new();
        if read_line_limited(reader, &mut header)? == 0 {
            return Ok(None);
        }
        if !header.ends_with('\n') {
            return Ok(None); // 单条头超长
        }
        header_bytes += header.len();
        if header_bytes > MAX_HEADER_BYTES {
            return Ok(None);
        }

        let header = header.trim_end();
        if header.is_empty() {
            break; // 头部结束
        }

        if let Some((name, value)) = header.split_once(':') {
            let value = value.trim();
            // 头部名大小写不敏感。
            match name.to_ascii_lowercase().as_str() {
                "authorization" => auth = Some(value.to_string()),
                "host" => host = Some(value.to_string()),
                // 解析不了就判为畸形，**不要**当成 0：那会把请求体留在流里，
                // 被 keep-alive 的下一轮当成新请求来解析（请求走私的经典形态）。
                "content-length" => match value.parse::<usize>() {
                    Ok(n) => content_length = n,
                    Err(_) => return Ok(None),
                },
                // 分块编码这里不实现。默默忽略等于漏读请求体，同样会让残留字节
                // 被当成下一个请求，所以一律拒绝而不是放过。
                "transfer-encoding" => return Ok(None),
                "connection" => close = value.eq_ignore_ascii_case("close"),
                _ => {}
            }
        }
    }

    if content_length > MAX_BODY {
        return Ok(None);
    }
    let mut body = vec![0u8; content_length];
    if content_length > 0 {
        reader.read_exact(&mut body)?;
    }

    Ok(Some(Request {
        method: method.to_string(),
        path,
        query,
        auth,
        host,
        body,
        close,
    }))
}

struct Response {
    status: &'static str,
    content_type: &'static str,
    body: Vec<u8>,
    /// 额外头，目前只用于封面的 ETag / Cache-Control。
    extra: Vec<String>,
}

impl Response {
    fn new(status: &'static str, content_type: &'static str, body: Vec<u8>) -> Self {
        Self {
            status,
            content_type,
            body,
            extra: Vec::new(),
        }
    }

    fn json(body: String) -> Self {
        Self::new(
            "200 OK",
            "application/json; charset=utf-8",
            body.into_bytes(),
        )
    }

    fn text(status: &'static str, msg: &str) -> Self {
        Self::new(status, "text/plain; charset=utf-8", msg.as_bytes().to_vec())
    }
}

fn route(req: &Request, token: &str, media: &Remote, bound: Option<Ipv4Addr>) -> Response {
    // DNS rebinding 的纵深防御。token 本来就挡住了这条路（重绑之后攻击者的脚本
    // 拿不到 localStorage 里的 token，API 全是 401），但校验 Host 只要一个 if，
    // 而且能在 401 之前就把请求挡掉。
    if !host_allowed(req, bound) {
        return Response::text("400 Bad Request", "Host 不匹配");
    }

    // 首页不校验 token：前端要先加载出来，才能从 URL 里取 token 存进 localStorage。
    // 它是纯静态资源，不含任何状态或控制能力。
    if req.path == "/" && req.method == "GET" {
        return Response::new("200 OK", "text/html; charset=utf-8", INDEX_HTML.into());
    }

    if !authorized(req, token) {
        return Response::text("401 Unauthorized", "需要 token");
    }

    match (req.method.as_str(), req.path.as_str()) {
        ("GET", "/api/state") => match media.call(Command::Snapshot) {
            Ok(Reply::Snapshot(s)) => Response::json(state_json(&s)),
            Ok(Reply::Error(e)) => Response::json(error_json(&e)),
            _ => Response::json(error_json("媒体线程返回了意外的回复")),
        },

        ("POST", "/api/cmd") => {
            let action = json_str(&req.body, "action").unwrap_or_default();
            let cmd = match action.as_str() {
                "next" => Command::Next,
                "prev" => Command::Prev,
                "playpause" => Command::TogglePlayPause,
                // 重播当前曲目。restart 只跳回起点，replay 跳回后顺带播放。
                "restart" => Command::Restart,
                "replay" => Command::Replay,
                // seek 需要 position 参数（秒）。
                "seek" => match json_num(&req.body, "position") {
                    Some(secs) => Command::Seek(secs),
                    None => return Response::text("400 Bad Request", "seek 需要 position"),
                },
                _ => return Response::text("400 Bad Request", "未知的 action"),
            };
            accepted_response(media.call(cmd))
        }

        ("POST", "/api/volume") => {
            // 两种载荷：{"level":0.5} 调音量，{"mute":true} 切静音。
            if let Some(level) = json_num(&req.body, "level") {
                accepted_response(media.call(Command::SetVolume(level as f32)))
            } else if let Some(mute) = json_bool(&req.body, "mute") {
                accepted_response(media.call(Command::SetMute(mute)))
            } else {
                Response::text("400 Bad Request", "需要 level 或 mute")
            }
        }

        ("GET", "/api/thumbnail") => match media.call(Command::Thumbnail) {
            Ok(Reply::Thumbnail(Some((tag, bytes)))) => {
                // SMTC 缩略图实测是 JPEG/PNG，两者都由浏览器按实际内容解码，
                // 所以统一标 image/jpeg 不影响显示。
                //
                // 注意这些字节来自媒体文件里内嵌的封面，属于外部内容。正因为浏览器
                // 会做内容嗅探，才**必须**带上 nosniff（在 write_response 里统一加），
                // 否则一段构造过的"封面"可能被嗅探成 HTML 执行掉。
                let mut r = Response::new("200 OK", "image/jpeg", bytes);
                // ETag 是图片内容的哈希，前端靠它判断图是否真的换了。
                r.extra.push(format!("ETag: {tag}"));
                // 不让浏览器缓存：判重交给 ETag，否则换歌时可能吃到旧图。
                r.extra.push("Cache-Control: no-store".into());
                r
            }
            _ => Response::text("404 Not Found", "无封面"),
        },

        ("GET", "/api/sessions") => match media.call(Command::ListSessions) {
            Ok(Reply::Sessions(list)) => Response::json(sessions_json(&list)),
            Ok(Reply::Error(e)) => Response::json(error_json(&e)),
            _ => Response::json(error_json("媒体线程返回了意外的回复")),
        },

        _ => Response::text("404 Not Found", "无此路径"),
    }
}

fn accepted_response(reply: std::result::Result<Reply, String>) -> Response {
    match reply {
        // accepted=false 表示 QQ音乐 拒绝了这条命令，不是错误。如实回传，
        // 前端可以据此提示"应用未接受"而不是假装成功。
        Ok(Reply::Accepted(ok)) => Response::json(format!("{{\"accepted\":{ok}}}")),
        Ok(Reply::Error(e)) => Response::json(error_json(&e)),
        Err(e) => Response::json(error_json(&e)),
        _ => Response::json(error_json("媒体线程返回了意外的回复")),
    }
}

/// 校验 `Host` 是否指向我们实际绑定的地址。
///
/// 防的是 DNS rebinding：攻击者用自己控制的域名先解析到自己的服务器，等页面加载完
/// 再把该域名重绑到 `192.168.x.x`，此后页面里的脚本就能对本服务发同源请求。那种
/// 请求的 `Host` 是攻击者的域名，而浏览器不允许脚本伪造 `Host`，所以只认"IP:端口"
/// 字面量就能把它挡在外面。
///
/// 允许缺省 `Host`：`remote.exe` 之外的手写客户端可能不发，而它们本来就不是浏览器
/// 里的脚本 —— rebinding 攻击的前提正是"跑在浏览器里"。
fn host_allowed(req: &Request, bound: Option<Ipv4Addr>) -> bool {
    let (Some(host), Some(ip)) = (req.host.as_deref(), bound) else {
        return true; // 没有 Host，或拿不到本地地址：跳过这项检查
    };

    // 端口可以省略，也可以带上；只比较主机部分。IPv6 字面量不会出现 ——
    // 服务端只绑 IPv4。
    let name = host.rsplit_once(':').map_or(host, |(h, _)| h);
    name == ip.to_string()
}

/// token 校验。接受 `Authorization: Bearer <token>` 或查询参数 `?t=<token>`。
///
/// 查询参数那条是为了首次访问：用户从控制台复制带 token 的 URL 打开，
/// 之后前端把 token 存进 localStorage，改走 Authorization 头。
fn authorized(req: &Request, token: &str) -> bool {
    let bearer = req
        .auth
        .as_deref()
        .and_then(|a| a.strip_prefix("Bearer "))
        .map(str::trim);
    if bearer.is_some_and(|b| constant_time_eq(b.as_bytes(), token.as_bytes())) {
        return true;
    }

    req.query
        .split('&')
        .filter_map(|pair| pair.strip_prefix("t="))
        .any(|v| constant_time_eq(v.as_bytes(), token.as_bytes()))
}

/// 常数时间比较，避免按字节提前返回而泄漏 token 前缀。
fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    // 累积异或后再判断，循环次数与内容无关。
    a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

fn write_response(w: &mut TcpStream, resp: &Response, close: bool) -> std::io::Result<()> {
    let mut head = format!(
        "HTTP/1.1 {}\r\nContent-Type: {}\r\nContent-Length: {}\r\nConnection: {}\r\n\
         X-Content-Type-Options: nosniff\r\n\
         Referrer-Policy: no-referrer\r\n\
         Content-Security-Policy: default-src 'none'; img-src 'self' blob:; \
style-src 'unsafe-inline'; script-src 'unsafe-inline'; connect-src 'self'\r\n",
        resp.status,
        resp.content_type,
        resp.body.len(),
        if close { "close" } else { "keep-alive" }
    );
    for line in &resp.extra {
        head.push_str(line);
        head.push_str("\r\n");
    }
    head.push_str("\r\n");

    w.write_all(head.as_bytes())?;
    w.write_all(&resp.body)?;
    w.flush()
}

// ---- JSON 输出 ----
// 字段固定且很少，手写比引 serde 划算。

fn state_json(s: &Snapshot) -> String {
    let mut out = String::from("{");
    out.push_str(&format!("\"present\":{},", s.present));
    // 前端据此显示降级提示：mediakey 模式下没有元数据，按键也是全局的。
    out.push_str(&format!("\"mode\":\"{}\",", s.mode.as_str()));
    out.push_str(&format!("\"matched\":{},", s.matched_target));
    out.push_str(&format!("\"playing\":{},", s.playing));
    out.push_str(&format!("\"aumid\":{},", quote(&s.aumid)));
    out.push_str(&format!("\"title\":{},", quote(&s.title)));
    out.push_str(&format!("\"artist\":{},", quote(&s.artist)));
    out.push_str(&format!("\"album\":{},", quote(&s.album)));
    out.push_str(&format!("\"artTag\":{},", quote(&s.art_tag)));
    // 位置已在服务端按 LastUpdatedTime 外推过 —— 浏览器在另一台机器上，
    // 用它自己的时钟算会引入两机时钟偏移。
    match s.position {
        Some(p) => out.push_str(&format!("\"position\":{p:.3},")),
        None => out.push_str("\"position\":null,"),
    }
    match s.duration {
        Some(d) => out.push_str(&format!("\"duration\":{d:.3},")),
        None => out.push_str("\"duration\":null,"),
    }
    out.push_str(&format!("\"canSeek\":{},", s.can_seek));
    // 音量为 null 表示 QQ音乐 当前没有音频会话（没出声），前端据此置灰滑杆。
    match s.volume {
        Some(v) => out.push_str(&format!("\"volume\":{v:.4},")),
        None => out.push_str("\"volume\":null,"),
    }
    match s.muted {
        Some(m) => out.push_str(&format!("\"muted\":{m}")),
        None => out.push_str("\"muted\":null"),
    }
    out.push('}');
    out
}

fn sessions_json(list: &[crate::media::SessionInfo]) -> String {
    let items: Vec<String> = list
        .iter()
        .map(|s| {
            format!(
                "{{\"aumid\":{},\"title\":{},\"artist\":{},\"status\":{},\"current\":{}}}",
                quote(&s.aumid),
                quote(&s.title),
                quote(&s.artist),
                quote(&s.status),
                s.is_current
            )
        })
        .collect();
    format!("{{\"sessions\":[{}]}}", items.join(","))
}

fn error_json(msg: &str) -> String {
    format!("{{\"error\":{}}}", quote(msg))
}

/// JSON 字符串转义。曲名里出现引号、反斜杠、控制字符都得处理；
/// QQ音乐 的曲目元数据是任意用户内容，不能假设它干净。
fn quote(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            // 其余控制字符走 \u 转义，否则会产出非法 JSON。
            c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

// ---- JSON 输入 ----
// 前端发的载荷形状固定（单层、字段已知），这里做的是定向提取而非通用解析。

/// 取字符串字段。不处理转义 —— action 取值是我们自己定的枚举，没有转义。
fn json_str(body: &[u8], key: &str) -> Option<String> {
    let s = std::str::from_utf8(body).ok()?;
    let needle = format!("\"{key}\"");
    let rest = &s[s.find(&needle)? + needle.len()..];
    let rest = rest.trim_start().strip_prefix(':')?.trim_start();
    let rest = rest.strip_prefix('"')?;
    let end = rest.find('"')?;
    Some(rest[..end].to_string())
}

fn json_num(body: &[u8], key: &str) -> Option<f64> {
    let s = std::str::from_utf8(body).ok()?;
    let needle = format!("\"{key}\"");
    let rest = &s[s.find(&needle)? + needle.len()..];
    let rest = rest.trim_start().strip_prefix(':')?.trim_start();
    let end = rest
        .find(|c: char| !matches!(c, '0'..='9' | '.' | '-' | '+' | 'e' | 'E'))
        .unwrap_or(rest.len());
    rest[..end].parse().ok()
}

fn json_bool(body: &[u8], key: &str) -> Option<bool> {
    let s = std::str::from_utf8(body).ok()?;
    let needle = format!("\"{key}\"");
    let rest = &s[s.find(&needle)? + needle.len()..];
    let rest = rest.trim_start().strip_prefix(':')?.trim_start();
    if rest.starts_with("true") {
        Some(true)
    } else if rest.starts_with("false") {
        Some(false)
    } else {
        None
    }
}

/// 绑定监听口。
pub fn bind(addr: SocketAddr) -> std::io::Result<TcpListener> {
    TcpListener::bind(addr)
}
