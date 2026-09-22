//! 手写的最小 HTTP/1.1 服务端。
//!
//! 只需要伺候一个自家前端：GET 几个 JSON、POST 几个命令、吐一张封面图。为此引入
//! axum 会连带 tokio 一整套，与本项目零依赖的取向不符，也没有对应的收益。
//!
//! 每个连接一个线程。并发量是"家里两三台设备"这个量级，线程池都算过度设计。

use crate::media::{Command, Remote, Reply, Snapshot};
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::time::Duration;

/// 内嵌前端。单文件，改完重新编译即可，不必操心运行时的相对路径。
const INDEX_HTML: &str = include_str!("../web/index.html");

/// 请求体上限。命令类请求都只有几十字节，给足余量即可。
const MAX_BODY: usize = 4 * 1024;
/// 请求头上限，防着慢速攻击和畸形请求占着线程。
const MAX_HEADER_BYTES: usize = 16 * 1024;
/// 读写超时。浏览器的 keep-alive 连接闲置后由超时收掉。
const IO_TIMEOUT: Duration = Duration::from_secs(30);

pub struct Server {
    pub listener: TcpListener,
    pub token: String,
    pub media: Remote,
}

impl Server {
    /// 接受连接并逐个处理，永不返回。
    pub fn serve(self) {
        for stream in self.listener.incoming() {
            let Ok(stream) = stream else { continue };
            let token = self.token.clone();
            let media = self.media.clone();
            // 单个连接出错不该影响整个服务，所以每个连接独立线程 + 忽略错误。
            std::thread::spawn(move || {
                let _ = stream.set_read_timeout(Some(IO_TIMEOUT));
                let _ = stream.set_write_timeout(Some(IO_TIMEOUT));
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

    // keep-alive：同一连接上串行处理多个请求，前端每秒轮询时省掉握手。
    loop {
        let req = match read_request(&mut reader) {
            Ok(Some(r)) => r,
            // 连接正常关闭或请求畸形，都直接收摊。
            _ => return,
        };

        let resp = route(&req, token, media);
        if write_response(&mut writer, &resp, req.close).is_err() {
            return;
        }
        // 客户端说了 close 就关掉 —— 它可能正在等对端关闭以判断响应结束。
        if req.close {
            return;
        }
    }
}

fn read_request(reader: &mut BufReader<TcpStream>) -> std::io::Result<Option<Request>> {
    let mut line = String::new();
    // 读到 0 字节表示对端关闭了连接。
    if reader.read_line(&mut line)? == 0 {
        return Ok(None);
    }

    let mut parts = line.split_whitespace();
    let (Some(method), Some(target)) = (parts.next(), parts.next()) else {
        return Ok(None);
    };
    let (path, query) = match target.split_once('?') {
        Some((p, q)) => (p.to_string(), q.to_string()),
        None => (target.to_string(), String::new()),
    };

    let mut auth = None;
    let mut content_length = 0usize;
    let mut close = false;
    let mut header_bytes = line.len();

    loop {
        let mut header = String::new();
        if reader.read_line(&mut header)? == 0 {
            return Ok(None);
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
                "content-length" => content_length = value.parse().unwrap_or(0),
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
        Self::new("200 OK", "application/json; charset=utf-8", body.into_bytes())
    }

    fn text(status: &'static str, msg: &str) -> Self {
        Self::new(status, "text/plain; charset=utf-8", msg.as_bytes().to_vec())
    }
}

fn route(req: &Request, token: &str, media: &Remote) -> Response {
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
                // SMTC 缩略图实测是 JPEG/PNG，浏览器按内容嗅探，标 image/jpeg 足够。
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
        "HTTP/1.1 {}\r\nContent-Type: {}\r\nContent-Length: {}\r\nConnection: {}\r\n",
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
