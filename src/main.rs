//! cross-next：在局域网里用浏览器遥控这台机器上的 QQ音乐。
//!
//! 必须在已登录的交互桌面会话里运行 —— SMTC 会话按 Windows 登录会话隔离，
//! 做成服务或从 SSH 启动都会拿不到会话。详见 README。
//!
//! 编成窗口子系统（见下方属性）：这个进程要在放音乐那台机器上常开，不该占一个
//! 控制台窗口。界面落在托盘图标上，右键能取回访问地址。从命令行启动时
//! `AttachConsole` 借到父进程的控制台，横幅照旧可见 —— 首次配置一定是那么跑的。
//!
//! 代价是：**双击运行时所有 `println!` / `eprintln!` 都无声**。所以启动期的错误
//! 一律走 `ui::fail`（没有控制台时弹消息框），否则用户看到的是"点了没反应"。

#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use cross_next::{http, jsonlite::field, media, net, tray, ui};
use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::path::PathBuf;

const DEFAULT_PORT: u16 = 8770;
const DEFAULT_TARGET: &str = "qqmusic";
/// 消息框标题。
const TITLE: &str = "cross-next";

struct Config {
    /// 匹配 SMTC AUMID / 进程名的子串，不区分大小写。
    target: String,
    port: u16,
    /// 留空则自动挑一个内网 IPv4。
    bind: String,
    token: String,
}

fn main() {
    // 必须在创建任何窗口（消息框、托盘）之前。
    ui::init_dpi();

    // 更新之后确认跑的是哪一份 —— exe 覆盖了但旧进程还在跑时，文件时间戳看不出来。
    // 排在 --stop 之前：两个都给的话先答版本，不会顺手把服务停掉。
    //
    // 注意脚本**不要**读这个输出：窗口子系统程序在 PowerShell 里用 `&` 调用抓不到
    // stdout，而且没有控制台时 ui::report 会弹模态框把脚本永久挂住。比文件哈希。
    if std::env::args()
        .skip(1)
        .any(|a| a == "--version" || a == "-V")
    {
        ui::report(TITLE, &format!("cross-next {}", ui::VERSION));
        return;
    }

    // `--stop` 只关掉在跑的实例然后退出，不启动新的。为更新准备：运行中的 exe
    // 被系统锁着，覆盖之前必须先让它退出。手点托盘的「退出」效果相同，但没法写脚本。
    //
    // 刻意不做成「启动时自动接管」（`listen.exe` 那套）：那个进程没有界面，用户看不见
    // 也关不掉，接管是唯一体面的出路；服务端有托盘图标，静默顶掉一个正在服务的实例
    // 是帮倒忙，还会破坏下面 AddrInUse 提示里「改 port 再开一个」的用法。
    if std::env::args().skip(1).any(|a| a == "--stop") {
        let msg = if tray::stop_running() {
            "已关闭正在运行的 cross-next。"
        } else {
            "没有正在运行的 cross-next。"
        };
        ui::report(TITLE, msg);
        return;
    }

    let path = config_path();
    let (config, created) = match load_or_create(&path) {
        Ok(c) => c,
        Err(e) => {
            ui::fail(TITLE, &format!("配置读写失败（{}）：{e}", path.display()));
            return;
        }
    };

    let addr = match resolve_addr(&config) {
        Ok(a) => a,
        Err(e) => {
            ui::fail(TITLE, &e);
            return;
        }
    };

    let listener = match http::bind(addr) {
        Ok(l) => l,
        Err(e) => {
            // 端口占用几乎总是"已经开着一个了" —— 这是本程序的单实例判据，
            // 所以说清楚，不要只丢一句系统错误。
            let hint = if e.kind() == std::io::ErrorKind::AddrInUse {
                format!(
                    "\n\n端口被占用 —— 通常是已经开着一个 cross-next 了（看托盘图标）。\n\n\
                     要换掉它就先 cross-next.exe --stop，或从托盘退出。\n\
                     确实要同时开两个就改 {} 里的 port。",
                    path.display()
                )
            } else {
                String::new()
            };
            ui::fail(TITLE, &format!("绑定 {addr} 失败：{e}{hint}"));
            return;
        }
    };

    // 媒体线程独占一个 MTA 单元，HTTP 线程通过通道跟它说话。
    let remote = media::spawn(config.target.clone());

    // 这些只在从命令行启动时可见（窗口子系统下双击没有控制台），正好用于首次配置。
    println!("cross-next {} 已启动", ui::VERSION);
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
    println!("  从托盘图标退出，或 Ctrl+C。");

    // 首次运行且无控制台：横幅没人看见，而 token 是新生成的，用户无从得知访问地址。
    // 弹一次说清楚。之后再启动就不弹了 —— 地址随时能从托盘菜单取回。
    if created && !ui::has_console() {
        ui::box_message(
            TITLE,
            &format!(
                "已生成配置：{}\n\n在同一局域网的浏览器里打开：\n\n\
                 http://{addr}/?t={}\n\n\
                 这个地址随时可以从托盘图标的右键菜单取回。",
                path.display(),
                config.token
            ),
            false,
        );
    }

    // token 要在两处用：服务端拿去校验，托盘拿去拼访问地址。这里复制一份给托盘。
    let token = config.token.clone();

    // serve() 永不返回，而托盘要占着主线程跑消息循环，所以服务端搬到后台线程。
    // 它是进程的主要工作，起不来就没有继续的意义。
    std::thread::Builder::new()
        .name("http-accept".into())
        .spawn(move || {
            http::Server {
                listener,
                token: config.token,
                media: remote,
            }
            .serve();
        })
        .expect("HTTP 线程启动失败");

    // 用户从托盘选「退出」时返回，进程随之结束（HTTP 线程是随进程走的）。
    tray::run(addr, &token);
}

/// 配置放在 exe 同目录，这样绿色版拷到哪都能带着走。
fn config_path() -> PathBuf {
    std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(|d| d.join("config.json")))
        .unwrap_or_else(|| PathBuf::from("config.json"))
}

/// 读配置，不存在则生成。
///
/// 第二个返回值是「token 是新生成的」—— 既包括首次运行，也包括用户清空了 token
/// 让它重新生成。两种情况下用户都不知道当前的访问地址，调用方要负责告知。
fn load_or_create(path: &PathBuf) -> std::io::Result<(Config, bool)> {
    if let Ok(text) = std::fs::read_to_string(path) {
        let existing = field(&text, "token").unwrap_or_default();
        // token 缺失或被清空时补一个，而不是裸奔。
        let fresh = existing.is_empty();
        let token = if fresh { new_token() } else { existing };

        let config = Config {
            target: field(&text, "target").unwrap_or_else(|| DEFAULT_TARGET.into()),
            port: field(&text, "port")
                .and_then(|v| v.parse().ok())
                .unwrap_or(DEFAULT_PORT),
            bind: field(&text, "bind").unwrap_or_default(),
            token,
        };

        // 补写回去，下次启动就稳定了。
        if fresh {
            write_config(path, &config)?;
        }
        return Ok((config, fresh));
    }

    let config = Config {
        target: DEFAULT_TARGET.into(),
        port: DEFAULT_PORT,
        bind: String::new(),
        token: new_token(),
    };
    write_config(path, &config)?;
    println!("已生成配置：{}", path.display());
    Ok((config, true))
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
