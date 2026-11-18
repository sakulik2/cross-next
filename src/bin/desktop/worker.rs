//! 网络线程：发命令、轮询状态、拉封面。UI 线程从不碰网络。
//!
//! 客户端超时是 2 秒，服务端一断，任何一次请求都可能卡满这么久。放在消息循环里
//! 就是窗口拖不动、按钮按不下。所以全部收到这一个线程，结果攒进收件箱，再
//! `PostMessage` 通知 UI 线程来取。
//!
//! 只开一个线程，命令和轮询串行：局域网上一次往返几毫秒，命令最多等一个轮询的
//! 时间；换来的是没有任何并发问题 —— 封面状态、退避计数都只在这里读写。

use crate::decode;
use crate::model::{self, Link, Mode};
use crate::pixels::{self, Image, Rgb};
use cross_next::client::{self, Config};
use std::sync::mpsc::{Receiver, RecvTimeoutError, Sender, channel};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime};
use windows::Win32::Foundation::{HWND, LPARAM, WPARAM};
use windows::Win32::System::Com::{COINIT_MULTITHREADED, CoInitializeEx};
use windows::Win32::UI::WindowsAndMessaging::PostMessageW;

/// UI 线程收到它就去收件箱取事件。
pub const WM_APP_EVENTS: u32 = windows::Win32::UI::WindowsAndMessaging::WM_APP + 1;

/// 正常轮询间隔。与网页一致。
const POLL: Duration = Duration::from_secs(1);
/// 连不上时的退避。服务端那台在休眠时没必要一秒敲一次门，但也别等太久 ——
/// 它醒来后界面该很快恢复。
const BACKOFF: [u64; 5] = [1, 2, 3, 5, 5];
/// 换歌后核对封面的时间点。SMTC 的缩略图有时比曲目元数据慢一拍，换歌瞬间取到的
/// 可能还是上一首的图，所以过一会儿再取几次；内容哈希没变就什么都不做。同网页。
const ART_CHECKS: [u64; 4] = [0, 200, 1200, 3000];
/// 切歌后补几次轮询，让新曲名尽快出来，别等下一个周期。同网页。
const AFTER_SKIP: [u64; 3] = [200, 600, 1400];
/// 封面解码后长边的上限。小组件在 200% 缩放下画 152 像素，任务栏缩略图更大一些。
const ART_MAX: u32 = 400;

pub enum Cmd {
    /// next / prev / playpause，原样作为 `remote.exe` 的命令名。
    Transport(&'static str),
    /// 相对音量，百分点。
    Volume(i32),
    Mute,
}

pub struct Cover {
    /// 方形，预乘 BGRA。
    pub img: Image,
    /// `(光晕, 底色)`，取不出时 `None`。
    pub colors: Option<(Rgb, Rgb)>,
}

pub enum Event {
    Link(Link),
    /// 封面换了。`None` 表示现在没有封面。
    Cover(Option<Arc<Cover>>),
    /// 一次性的提示（命令失败之类），在界面上闪一下。
    Notice(String),
}

pub type Inbox = Arc<Mutex<Vec<Event>>>;

/// 起线程。返回发命令的通道和收件箱。
pub fn spawn(hwnd: HWND, config: Arc<Mutex<Config>>) -> (Sender<Cmd>, Inbox) {
    let (tx, rx) = channel();
    let inbox: Inbox = Arc::default();
    let out = Outbox {
        // HWND 不是 Send，跨线程传句柄值本身。
        hwnd: hwnd.0 as isize,
        inbox: Arc::clone(&inbox),
    };
    std::thread::Builder::new()
        .name("network".into())
        .spawn(move || run(rx, config, out))
        .expect("起网络线程失败");
    (tx, inbox)
}

struct Outbox {
    hwnd: isize,
    inbox: Inbox,
}

impl Outbox {
    fn send(&self, e: Event) {
        self.inbox.lock().unwrap().push(e);
        let hwnd = HWND(self.hwnd as *mut _);
        let _ = unsafe { PostMessageW(Some(hwnd), WM_APP_EVENTS, WPARAM(0), LPARAM(0)) };
    }
}

/// 封面这一侧的状态。
#[derive(Default)]
struct Art {
    /// 上次看到的 artTag。`None` 表示当前不适用封面。
    tag: Option<String>,
    /// 当前显示的那张图的内容哈希。`None` 表示当前没有图。
    shown: Option<u64>,
    /// 待做的核对。
    checks: Vec<Instant>,
}

fn run(rx: Receiver<Cmd>, config: Arc<Mutex<Config>>, out: Outbox) {
    // WIC 要 COM。本线程独占，MTA 即可。
    let _ = unsafe { CoInitializeEx(None, COINIT_MULTITHREADED) }.ok();

    let mut stamp = config_stamp();
    let mut next_poll = Instant::now();
    let mut extra_polls: Vec<Instant> = Vec::new();
    let mut failures = 0usize;
    let mut art = Art::default();

    loop {
        let now = Instant::now();
        let deadline = art
            .checks
            .iter()
            .chain(&extra_polls)
            .copied()
            .fold(next_poll, Instant::min);

        match rx.recv_timeout(deadline.saturating_duration_since(now)) {
            Ok(first) => {
                let cfg = config.lock().unwrap().clone();
                let mut skipped = false;
                for cmd in coalesce(first, &rx) {
                    skipped |= matches!(cmd, Cmd::Transport("next" | "prev"));
                    if let Err(e) = execute(&cfg, &cmd) {
                        out.send(Event::Notice(e.lines().next().unwrap_or("").to_string()));
                    }
                }
                // 命令之后立刻刷新一次，界面别等下一个周期。
                next_poll = Instant::now();
                if skipped {
                    let t = Instant::now();
                    extra_polls = AFTER_SKIP
                        .iter()
                        .map(|ms| t + Duration::from_millis(*ms))
                        .collect();
                }
                continue;
            }
            Err(RecvTimeoutError::Timeout) => {}
            // UI 线程退出了，通道随之关闭。
            Err(RecvTimeoutError::Disconnected) => return,
        }

        let now = Instant::now();
        let poll_due = now >= next_poll || extra_polls.iter().any(|t| *t <= now);
        extra_polls.retain(|t| *t > now);

        if poll_due {
            // remote.json 改了就换配置 —— 填错 token 后改文件，界面自己恢复，
            // 不用重启。读坏了就继续用旧的。
            let s = config_stamp();
            if s != stamp {
                stamp = s;
                if let Ok(c) = client::load_config() {
                    *config.lock().unwrap() = c;
                    // 换了服务端，旧封面不再作数。
                    art.tag = None;
                }
            }

            let cfg = config.lock().unwrap().clone();
            let link = poll(&cfg);
            failures = match link {
                Link::Unreachable => failures + 1,
                _ => 0,
            };
            next_poll = now
                + match failures {
                    0 => POLL,
                    n => Duration::from_secs(BACKOFF[(n - 1).min(BACKOFF.len() - 1)]),
                };

            if let Link::Ok(s) = &link {
                track_art(&mut art, s, now, &out);
            }
            out.send(Event::Link(link));
        }

        if art.checks.iter().any(|t| *t <= now) {
            art.checks.retain(|t| *t > now);
            let cfg = config.lock().unwrap().clone();
            fetch_art(&cfg, &mut art, &out);
        }
    }
}

/// 把排队的命令取空，相邻的音量调整合并成一次。
///
/// 按住音量组合键时键盘自动重复每秒二三十次，而每次音量调整要先读再写两个往返。
/// 不合并的话队列越积越长，松手后音量还在一格一格地走。
fn coalesce(first: Cmd, rx: &Receiver<Cmd>) -> Vec<Cmd> {
    let mut out = vec![first];
    while let Ok(cmd) = rx.try_recv() {
        if let (Some(Cmd::Volume(acc)), Cmd::Volume(d)) = (out.last_mut(), &cmd) {
            *acc += d;
            continue;
        }
        out.push(cmd);
    }
    out
}

fn execute(cfg: &Config, cmd: &Cmd) -> Result<(), String> {
    let args: Vec<String> = match cmd {
        Cmd::Transport(a) => vec![(*a).into()],
        Cmd::Volume(0) => return Ok(()),
        Cmd::Volume(d) => vec!["vol".into(), format!("{d:+}")],
        Cmd::Mute => vec!["mute".into()],
    };
    client::dispatch(cfg, &args)
}

fn poll(cfg: &Config) -> Link {
    match client::fetch(cfg, "GET", "/api/state", None) {
        Err(_) => Link::Unreachable,
        Ok((401, _)) => Link::Unauthorized,
        Ok((200, body)) => match model::parse_state(&String::from_utf8_lossy(&body)) {
            Ok(s) => Link::Ok(s),
            Err(e) => Link::Failed(e),
        },
        Ok((code, _)) => Link::Failed(format!("服务端返回 HTTP {code}")),
    }
}

/// 根据新状态决定要不要（重新）取封面。
fn track_art(art: &mut Art, s: &model::State, now: Instant, out: &Outbox) {
    let applies = s.present && s.mode == Mode::Smtc;
    if !applies {
        art.tag = None;
        art.checks.clear();
        if art.shown.take().is_some() {
            out.send(Event::Cover(None));
        }
        return;
    }
    if art.tag.as_deref() != Some(s.art_tag.as_str()) {
        art.tag = Some(s.art_tag.clone());
        art.checks = ART_CHECKS
            .iter()
            .map(|ms| now + Duration::from_millis(*ms))
            .collect();
    }
}

fn fetch_art(cfg: &Config, art: &mut Art, out: &Outbox) {
    let bytes = match client::fetch(cfg, "GET", "/api/thumbnail", None) {
        Ok((200, b)) if !b.is_empty() => Some(b),
        // 404 = 这首没有封面。
        Ok(_) => None,
        // 传输失败：这次核对作废，不改变现状。
        Err(_) => return,
    };

    let hash = bytes.as_deref().map(fnv);
    if hash == art.shown {
        return;
    }

    let cover = bytes.and_then(|b| decode::decode(&b, ART_MAX)).map(|img| {
        let img = img.crop_square();
        let colors = pixels::pick_colors(&img);
        Arc::new(Cover { img, colors })
    });
    // 解不出来的图按「没有封面」处理，但照样记下哈希，免得每次核对都重解一遍。
    art.shown = hash;
    out.send(Event::Cover(cover));
}

/// FNV-1a，判断两张封面字节是否相同。不需要抗碰撞。
fn fnv(bytes: &[u8]) -> u64 {
    bytes.iter().fold(0xcbf2_9ce4_8422_2325, |h, b| {
        (h ^ *b as u64).wrapping_mul(0x0100_0000_01b3)
    })
}

/// `remote.json` 的修改时间，用来判断要不要重读。
fn config_stamp() -> Option<SystemTime> {
    std::fs::metadata(client::config_path())
        .and_then(|m| m.modified())
        .ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn drain(cmds: Vec<Cmd>) -> Vec<String> {
        let (tx, rx) = channel();
        let mut it = cmds.into_iter();
        let first = it.next().unwrap();
        for c in it {
            tx.send(c).unwrap();
        }
        coalesce(first, &rx)
            .iter()
            .map(|c| match c {
                Cmd::Transport(a) => a.to_string(),
                Cmd::Volume(d) => format!("vol{d:+}"),
                Cmd::Mute => "mute".into(),
            })
            .collect()
    }

    #[test]
    fn coalesces_adjacent_volume_only() {
        assert_eq!(
            drain(vec![
                Cmd::Volume(5),
                Cmd::Volume(5),
                Cmd::Volume(-5),
                Cmd::Transport("next"),
                Cmd::Volume(5)
            ]),
            ["vol+5", "next", "vol+5"]
        );
        assert_eq!(
            drain(vec![Cmd::Transport("next"), Cmd::Transport("next")]),
            ["next", "next"]
        );
        assert_eq!(drain(vec![Cmd::Mute]), ["mute"]);
    }

    #[test]
    fn fnv_distinguishes() {
        assert_ne!(fnv(b"a"), fnv(b"b"));
        assert_eq!(fnv(b"abc"), fnv(b"abc"));
    }
}
