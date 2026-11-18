//! 客户端侧：连到另一台机器上的 cross-next 并发命令。
//!
//! `remote.exe`（一次性）和 `listen.exe`（常驻转发）共用这里。

use crate::jsonlite;
use std::io::{BufRead, Read, Write};
use std::net::TcpStream;
use std::path::PathBuf;
use std::time::Duration;

/// 连接和读写超时。本地网络 2 秒足够；设短一点是为了按键手感 ——
/// 服务器没开时不该让鼠标像卡住一样等着。
const TIMEOUT: Duration = Duration::from_secs(2);

/// `PartialEq` 用于热重载时判断配置是否真的变了（编辑器保存一次可能触发多次通知）。
#[derive(Clone, PartialEq)]
pub struct Config {
    pub host: String,
    pub port: u16,
    pub token: String,
}

/// 客户端配置放在 exe 同目录的 remote.json。
pub fn config_path() -> PathBuf {
    std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(|d| d.join("remote.json")))
        .unwrap_or_else(|| PathBuf::from("remote.json"))
}

/// 读配置。文件不存在时写一份模板并返回提示。
pub fn load_config() -> Result<Config, String> {
    let path = config_path();
    let text = std::fs::read_to_string(&path).map_err(|_| {
        // 没有配置就顺手写一份模板，比让用户自己猜格式友好。
        let template = "{\n  \"host\": \"192.168.1.1\",\n  \"port\": 8770,\n  \
                        \"token\": \"把服务端 config.json 里的 token 抄过来\"\n}\n";
        let _ = std::fs::write(&path, template);
        format!(
            "缺少配置，已生成模板：\n{}\n\n把服务端 config.json 里的 host 和 token 填进去。",
            path.display()
        )
    })?;

    let token = jsonlite::field(&text, "token").unwrap_or_default();
    if token.is_empty() || token.starts_with("把服务端") {
        return Err(format!("{} 里的 token 还没填。", path.display()));
    }

    Ok(Config {
        host: jsonlite::field(&text, "host").ok_or("配置里缺少 host")?,
        port: jsonlite::field(&text, "port")
            .and_then(|v| v.parse().ok())
            .unwrap_or(8770),
        token,
    })
}

/// 执行一条命令。命令名与 `remote.exe` 的参数一致。
pub fn dispatch(config: &Config, args: &[String]) -> Result<(), String> {
    match args[0].as_str() {
        "next" | "prev" | "playpause" => {
            let body = format!("{{\"action\":\"{}\"}}", args[0]);
            let resp = post(config, "/api/cmd", &body)?;
            // accepted=false 表示 QQ音乐 拒绝了命令，如实报出来。
            if resp.contains("\"accepted\":false") {
                return Err("QQ音乐 未接受这个操作".into());
            }
            report_error(&resp)
        }

        "mute" => {
            // 读当前状态再取反，这样一个按键就能来回切。
            let state = get(config, "/api/state")?;
            let muted = jsonlite::field(&state, "muted").as_deref() == Some("true");
            let resp = post(config, "/api/volume", &format!("{{\"mute\":{}}}", !muted))?;
            report_error(&resp)
        }

        "vol" => {
            let arg = args.get(1).ok_or("vol 需要一个参数，例如 +10 或 60")?;
            let level = resolve_volume(config, arg)?;
            let resp = post(config, "/api/volume", &format!("{{\"level\":{level:.4}}}"))?;
            report_error(&resp)
        }

        other => Err(format!("未知命令：{other}")),
    }
}

/// 把 `+10` / `-5` / `60` 解析成 0.0-1.0 的目标音量。
fn resolve_volume(config: &Config, arg: &str) -> Result<f32, String> {
    let relative = arg.starts_with('+') || arg.starts_with('-');
    let n: f32 = arg
        .parse()
        .map_err(|_| format!("音量参数无法解析：{arg}（用 +10、-5 或 60）"))?;

    let target = if relative {
        let state = get(config, "/api/state")?;
        // volume 为 null 表示 QQ音乐 当前没有音频会话，没有基准可加减。
        let current = jsonlite::number(&state, "volume")
            .ok_or("QQ音乐 当前没有音频会话，无法相对调整音量")? as f32;
        current * 100.0 + n
    } else {
        n
    };

    Ok((target / 100.0).clamp(0.0, 1.0))
}

/// 服务端把错误放在 JSON 的 error 字段里，HTTP 状态仍是 200。
fn report_error(resp: &str) -> Result<(), String> {
    match jsonlite::field(resp, "error") {
        Some(e) if !e.is_empty() => Err(e),
        _ => Ok(()),
    }
}

// ---- 极简 HTTP 客户端 ----
// 只需要发一个请求读一个响应，用 TcpStream 直接写比引 reqwest 划算得多。

pub fn get(config: &Config, path: &str) -> Result<String, String> {
    request(config, "GET", path, None)
}

pub fn post(config: &Config, path: &str, body: &str) -> Result<String, String> {
    request(config, "POST", path, Some(body))
}

fn request(
    config: &Config,
    method: &str,
    path: &str,
    body: Option<&str>,
) -> Result<String, String> {
    let (status, body) = fetch(config, method, path, body)?;

    if status == 401 {
        return Err(
            "token 无效。检查 remote.json 里的 token 和服务端 config.json 是否一致。".into(),
        );
    }
    if status != 200 {
        return Err(format!("服务端返回 HTTP {status}"));
    }

    Ok(String::from_utf8_lossy(&body).into_owned())
}

/// 浏览器里打开的完整地址（带 token）。
pub fn web_url(config: &Config) -> String {
    format!("http://{}:{}/?t={}", config.host, config.port, config.token)
}

/// 发一个请求，原样返回状态码与响应体字节。
///
/// `Err` 只表示**传输层**失败（连不上、超时、读写中断）；HTTP 层的 401、404
/// 都在 `Ok` 里，由调用方解释。`desktop.exe` 要靠这个区分「连不上」和「token 错」，
/// 拉封面时还要原始字节 —— 走 [`request`] 的话图片会被 `from_utf8_lossy` 毁掉。
pub fn fetch(
    config: &Config,
    method: &str,
    path: &str,
    body: Option<&str>,
) -> Result<(u16, Vec<u8>), String> {
    let addr = format!("{}:{}", config.host, config.port);
    let mut stream = connect(&addr)?;
    stream.set_read_timeout(Some(TIMEOUT)).ok();
    stream.set_write_timeout(Some(TIMEOUT)).ok();

    let mut req = format!(
        "{method} {path} HTTP/1.1\r\nHost: {addr}\r\n\
         Authorization: Bearer {}\r\nConnection: close\r\n",
        config.token
    );
    if let Some(b) = body {
        req.push_str(&format!(
            "Content-Type: application/json\r\nContent-Length: {}\r\n",
            b.len()
        ));
    }
    req.push_str("\r\n");
    if let Some(b) = body {
        req.push_str(b);
    }

    stream
        .write_all(req.as_bytes())
        .map_err(|e| format!("发送失败：{e}"))?;

    // 按 Content-Length 精确读，不依赖对端关闭连接 —— 服务端若是 keep-alive，
    // read_to_end 会一直等到超时。
    let mut reader = std::io::BufReader::new(&stream);

    let mut status_line = String::new();
    reader
        .read_line(&mut status_line)
        .map_err(|e| format!("读取失败：{e}"))?;

    // 状态行形如 `HTTP/1.1 200 OK`。
    let status = status_line
        .split_whitespace()
        .nth(1)
        .and_then(|s| s.parse::<u16>().ok())
        .unwrap_or(0);

    let mut content_length = 0usize;
    loop {
        let mut header = String::new();
        if reader
            .read_line(&mut header)
            .map_err(|e| format!("读取失败：{e}"))?
            == 0
        {
            break; // 对端提前关闭
        }
        let header = header.trim_end();
        if header.is_empty() {
            break; // 头部结束
        }
        if let Some((_, value)) = header
            .split_once(':')
            .filter(|(name, _)| name.eq_ignore_ascii_case("content-length"))
        {
            content_length = value.trim().parse().unwrap_or(0);
        }
    }

    let mut body = vec![0u8; content_length];
    if content_length > 0 {
        reader
            .read_exact(&mut body)
            .map_err(|e| format!("读取响应体失败：{e}"))?;
    }

    Ok((status, body))
}

/// 带超时的连接。
///
/// `TcpStream::connect` 自己没有超时：对端机器休眠或关机时，Windows 要等 SYN
/// 重传耗尽（约 21 秒）才放弃。按键的手感和常驻界面的轮询都等不起这么久。
/// `host` 可能是主机名，所以先解析，逐个地址试。
fn connect(addr: &str) -> Result<TcpStream, String> {
    use std::net::ToSocketAddrs;

    let hint = "cross-next 在那台机器上跑着吗？";
    let addrs = addr.to_socket_addrs().map_err(|e| {
        format!(
            "连不上 {addr}：{e}

{hint}"
        )
    })?;
    let mut last = None;
    for a in addrs {
        match TcpStream::connect_timeout(&a, TIMEOUT) {
            Ok(s) => return Ok(s),
            Err(e) => last = Some(e),
        }
    }
    Err(match last {
        Some(e) => format!(
            "连不上 {addr}：{e}

{hint}"
        ),
        None => format!(
            "连不上 {addr}：解析不出地址

{hint}"
        ),
    })
}
