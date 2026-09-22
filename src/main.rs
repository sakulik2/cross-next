//! cross-next：在局域网里用浏览器遥控这台机器上的 QQ音乐。
//!
//! 必须在已登录的交互桌面会话里运行 —— SMTC 会话按 Windows 登录会话隔离，
//! 做成服务或从 SSH 启动都会拿不到会话。详见 README。

use cross_next::{http, jsonlite::field, media, net};
use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::path::PathBuf;

const DEFAULT_PORT: u16 = 8770;
const DEFAULT_TARGET: &str = "qqmusic";

struct Config {
    /// 匹配 SMTC AUMID / 进程名的子串，不区分大小写。
    target: String,
    port: u16,
    /// 留空则自动挑一个内网 IPv4。
    bind: String,
    token: String,
}

fn main() {
    let path = config_path();
    let config = match load_or_create(&path) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("配置读写失败（{}）：{e}", path.display());
            return;
        }
    };

    let addr = match resolve_addr(&config) {
        Ok(a) => a,
        Err(e) => {
            eprintln!("{e}");
            return;
        }
    };

    let listener = match http::bind(addr) {
        Ok(l) => l,
        Err(e) => {
            eprintln!("绑定 {addr} 失败：{e}");
            if e.kind() == std::io::ErrorKind::AddrInUse {
                eprintln!("端口被占用。改 {} 里的 port 再试。", path.display());
            }
            return;
        }
    };

    // 媒体线程独占一个 MTA 单元，HTTP 线程通过通道跟它说话。
    let remote = media::spawn(config.target.clone());

    println!("cross-next 已启动");
    println!();
    println!("  在笔记本或手机浏览器打开：");
    println!("    http://{addr}/?t={}", config.token);
    println!();
    println!("  目标应用: {}   配置: {}", config.target, path.display());
    println!("  排查会话: http://{addr}/api/sessions?t={}", config.token);
    println!();
    println!("  这是明文 HTTP，token 只用于挡住同网段的误触，不防嗅探。");
    println!("  不要把这个端口转发到公网。");
    println!();
    println!("  Ctrl+C 退出。");

    http::Server {
        listener,
        token: config.token,
        media: remote,
    }
    .serve();
}

/// 配置放在 exe 同目录，这样绿色版拷到哪都能带着走。
fn config_path() -> PathBuf {
    std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(|d| d.join("config.json")))
        .unwrap_or_else(|| PathBuf::from("config.json"))
}

fn load_or_create(path: &PathBuf) -> std::io::Result<Config> {
    if let Ok(text) = std::fs::read_to_string(path) {
        let token = field(&text, "token").unwrap_or_default();
        // token 缺失或被清空时补一个，而不是裸奔。
        let token = if token.is_empty() { new_token() } else { token };

        let config = Config {
            target: field(&text, "target").unwrap_or_else(|| DEFAULT_TARGET.into()),
            port: field(&text, "port")
                .and_then(|v| v.parse().ok())
                .unwrap_or(DEFAULT_PORT),
            bind: field(&text, "bind").unwrap_or_default(),
            token,
        };

        // 补写回去，下次启动就稳定了。
        if field(&text, "token").unwrap_or_default().is_empty() {
            write_config(path, &config)?;
        }
        return Ok(config);
    }

    let config = Config {
        target: DEFAULT_TARGET.into(),
        port: DEFAULT_PORT,
        bind: String::new(),
        token: new_token(),
    };
    write_config(path, &config)?;
    println!("已生成配置：{}", path.display());
    Ok(config)
}

fn write_config(path: &PathBuf, c: &Config) -> std::io::Result<()> {
    // 手写 JSON。字段都是自己产生的，target/bind 不含需要转义的字符。
    let text = format!(
        "{{\n  \"_说明\": \"target 匹配 SMTC AUMID 与进程名的子串，不区分大小写；bind 留空则自动挑内网 IPv4\",\n  \
         \"target\": \"{}\",\n  \"port\": {},\n  \"bind\": \"{}\",\n  \"token\": \"{}\"\n}}\n",
        c.target, c.port, c.bind, c.token
    );
    std::fs::write(path, text)
}

/// 生成 32 字节的随机 token，十六进制表示。
///
/// 熵源用 Windows 的 `ProcessPrng`（对应 RtlGenRandom 的现代替代），
/// 它是系统 CSPRNG，不必自己攒时间戳之类的弱种子。
fn new_token() -> String {
    let mut bytes = [0u8; 32];
    // 文档保证 ProcessPrng 恒返回 TRUE。但这是整个鉴权的唯一熵源 ——
    // 万一那个保证不成立，静默的后果是 token 变成 64 个 0 且毫无征兆，
    // 所以宁可在这里直接崩掉。检查的代价为零。
    let ok = unsafe { windows::Win32::Security::Cryptography::ProcessPrng(&mut bytes) };
    assert!(ok.as_bool(), "ProcessPrng 失败，无法生成安全的 token");
    assert!(bytes.iter().any(|&b| b != 0), "熵源返回全零，拒绝使用");

    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// 决定监听地址。
fn resolve_addr(config: &Config) -> Result<SocketAddr, String> {
    if !config.bind.is_empty() {
        let ip: Ipv4Addr = config
            .bind
            .parse()
            .map_err(|_| format!("配置里的 bind 不是合法 IPv4：{}", config.bind))?;
        return Ok(SocketAddr::new(IpAddr::V4(ip), config.port));
    }

    let candidates = net::private_ipv4s();
    match candidates.split_first() {
        Some((first, rest)) => {
            if !rest.is_empty() {
                println!("发现多个内网地址，已优先选有默认网关的那个：");
                for ip in &candidates {
                    println!("  {ip}{}", if ip == first { "   <- 使用" } else { "" });
                }
                println!("  如果别的设备连不上，说明选错了网卡（WSL / Hyper-V 的");
                println!("  虚拟网卡也是 172.x 私网段，但局域网里路由不到）。");
                println!("  这时改配置里的 bind，填 ipconfig 里和路由器同网段的那个地址。");
                println!();
            }
            Ok(SocketAddr::new(IpAddr::V4(*first), config.port))
        }
        None => Err("未找到内网 IPv4 地址。确认已连上网络，\
                     或在配置里显式指定 bind。"
            .into()),
    }
}
