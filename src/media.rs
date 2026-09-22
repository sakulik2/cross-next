//! 媒体线程：独占一个 MTA 单元，持有 SMTC 会话管理器。
//!
//! 为什么要专门起一个线程而不是在 HTTP 线程里直接调：
//!   1. COM 单元初始化只做一次，不必操心每个 HTTP 工作线程的单元状态
//!   2. WinRT 对象不用跨线程传递，绕开所有 Send/Sync 的疑问
//!   3. windows-rs #2061 提到反复创建 SessionManager 会漏内存，复用同一个实例
//!
//! HTTP 线程通过 `Remote` 发命令，用一次性通道等回复。

use crate::keepawake;
use crate::mediakey;
use crate::volume;
use crate::winrt_block::{block_on, block_on_progress};
use std::sync::mpsc::{Receiver, Sender, channel};
use windows::Media::Control::{
    GlobalSystemMediaTransportControlsSession as Session,
    GlobalSystemMediaTransportControlsSessionManager as SessionManager,
    GlobalSystemMediaTransportControlsSessionPlaybackStatus as PlaybackStatus,
};
use windows::Storage::Streams::{Buffer, DataReader, InputStreamOptions};
use windows::Win32::System::Com::{COINIT_MULTITHREADED, CoInitializeEx};
use windows::core::Result;

/// 封面图最大读取字节数。SMTC 缩略图通常远小于此，设上限只为防意外。
const MAX_THUMBNAIL: u32 = 8 * 1024 * 1024;

/// 控制通路。SMTC 拿不到会话时自动降级到模拟媒体键。
#[derive(Clone, Copy, PartialEq, Default)]
pub enum Mode {
    /// 找不到目标，什么都做不了。
    #[default]
    None,
    /// 走 SMTC：定向控制，有完整元数据。
    Smtc,
    /// 走模拟媒体键：有些 QQ音乐 版本只接了老的 WM_APPCOMMAND 通路，
    /// 不注册 SMTC 会话。这时只能发全局媒体键，读不到任何元数据。
    MediaKey,
}

impl Mode {
    pub fn as_str(self) -> &'static str {
        match self {
            Mode::None => "none",
            Mode::Smtc => "smtc",
            Mode::MediaKey => "mediakey",
        }
    }
}

/// 一次状态快照。这是 HTTP 层唯一需要知道的媒体状态形状。
#[derive(Clone, Default)]
pub struct Snapshot {
    /// 是否找到了目标会话（QQ音乐）。false 时其余字段无意义。
    pub present: bool,
    /// 当前实际走的控制通路。前端据此决定要不要显示降级提示。
    pub mode: Mode,
    /// 命中的是配置的 target 还是回退到了系统当前会话。
    pub matched_target: bool,
    pub aumid: String,
    pub title: String,
    pub artist: String,
    pub album: String,
    /// 正在播放。前端据此决定播放键画 ▶ 还是 ⏸。
    pub playing: bool,
    /// 封面的内容标识，随曲目变化。前端用它判断要不要重取图。
    pub art_tag: String,
    /// 播放位置（秒），已按 LastUpdatedTime 外推到「现在」。
    /// None 表示这个会话不上报时间轴。
    pub position: Option<f64>,
    /// 总时长（秒）。None 同上。
    pub duration: Option<f64>,
    /// 应用是否允许拖动定位。
    pub can_seek: bool,
    /// QQ音乐 的音量 0.0-1.0；会话不存在时为 None（未出声时 Core Audio 里没有它）。
    pub volume: Option<f32>,
    pub muted: Option<bool>,
}

/// 探针用的单个会话描述。
pub struct SessionInfo {
    pub aumid: String,
    pub title: String,
    pub artist: String,
    pub status: String,
    pub is_current: bool,
}

pub enum Command {
    Snapshot,
    /// 上一首 / 下一首 / 播放暂停切换。
    Prev,
    Next,
    TogglePlayPause,
    /// 拖动到指定位置（秒）。
    Seek(f64),
    /// 内部心跳，仅用于刷新休眠抑制状态。不产生回复。
    Heartbeat,
    /// 设置 QQ音乐 进程音量。
    SetVolume(f32),
    SetMute(bool),
    /// 取封面字节。
    Thumbnail,
    /// 列出全部会话，供 /api/sessions 排查用。
    ListSessions,
}

pub enum Reply {
    Snapshot(Snapshot),
    /// 命令是否被应用接受。SMTC 的 Try* 返回 false 表示"应用拒绝"，
    /// 这不是错误，要如实告诉前端，不能当成功。
    Accepted(bool),
    /// (内容哈希, 字节)。哈希做 ETag，让前端能判断图是否真的换了。
    Thumbnail(Option<(String, Vec<u8>)>),
    Sessions(Vec<SessionInfo>),
    Error(String),
}

/// HTTP 线程持有的句柄。克隆廉价，可发给每个工作线程。
#[derive(Clone)]
pub struct Remote {
    tx: Sender<(Command, Sender<Reply>)>,
}

impl Remote {
    /// 发一条命令并等回复。媒体线程已退出时返回 Err。
    pub fn call(&self, cmd: Command) -> std::result::Result<Reply, String> {
        let (rtx, rrx) = channel();
        self.tx
            .send((cmd, rtx))
            .map_err(|_| "媒体线程已退出".to_string())?;
        rrx.recv().map_err(|_| "媒体线程无响应".to_string())
    }
}

/// 起媒体线程。`target` 是匹配 AUMID 的子串，不区分大小写。
pub fn spawn(target: String) -> Remote {
    let (tx, rx) = channel();
    // 心跳线程要往同一个队列里发，所以先克隆一份发送端给媒体线程带走。
    let tx_self = tx.clone();
    std::thread::Builder::new()
        .name("media".into())
        .spawn(move || run(rx, target, tx_self))
        .expect("媒体线程启动失败");
    Remote { tx }
}

/// 定时投心跳，让休眠抑制状态不依赖客户端轮询。
///
/// 20 秒足够及时 —— Windows 的最短空闲休眠是 1 分钟，这个间隔能在它决定睡之前
/// 多次刷新。发送端断开说明媒体线程没了，此时退出。
fn spawn_heartbeat(tx: Sender<(Command, Sender<Reply>)>) {
    std::thread::Builder::new()
        .name("heartbeat".into())
        .spawn(move || {
            loop {
                std::thread::sleep(std::time::Duration::from_secs(20));
                // 心跳不需要回复，但通道签名要求一个发送端；丢弃接收端即可。
                let (dummy, _) = channel();
                if tx.send((Command::Heartbeat, dummy)).is_err() {
                    return; // 媒体线程已退出
                }
            }
        })
        .expect("心跳线程启动失败");
}

fn run(
    rx: Receiver<(Command, Sender<Reply>)>,
    target: String,
    tx_self: Sender<(Command, Sender<Reply>)>,
) {
    // MTA：block_on 靠 Condvar 阻塞等待，STA 上会因为缺消息泵而死锁。
    unsafe {
        // 已初始化过返回 S_FALSE，不是错误。
        let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
    }

    // 整个进程生命周期内复用这一个 manager（见模块头注释里的内存泄漏原因）。
    let manager = match block_on(match SessionManager::RequestAsync() {
        Ok(op) => op,
        Err(e) => {
            drain_with_error(rx, &smtc_error_hint(&e));
            return;
        }
    }) {
        Ok(m) => m,
        Err(e) => {
            drain_with_error(rx, &smtc_error_hint(&e));
            return;
        }
    };

    // 播放期间阻止系统休眠。住在媒体线程里是因为 SetThreadExecutionState 按线程
    // 生效，而这个线程活整个进程生命周期。
    let mut awake = keepawake::KeepAwake::new();

    // 自带心跳：不能只在浏览器轮询时更新休眠抑制状态 —— 关掉浏览器后状态会冻结在
    // 最后一次的值，若那次是「播放中」，机器就永久不睡了。
    spawn_heartbeat(tx_self);

    for (cmd, reply) in rx {
        let out = match cmd {
            Command::Snapshot => match snapshot(&manager, &target) {
                Ok(s) => {
                    awake.set(s.playing);
                    Reply::Snapshot(s)
                }
                Err(e) => Reply::Error(e.message()),
            },
            // 心跳：只为刷新休眠抑制状态，不需要回复。
            Command::Heartbeat => {
                if let Ok(s) = snapshot(&manager, &target) {
                    awake.set(s.playing);
                }
                continue;
            }
            Command::Prev => transport(&manager, &target, Transport::Prev),
            Command::Next => transport(&manager, &target, Transport::Next),
            Command::TogglePlayPause => transport(&manager, &target, Transport::Toggle),
            Command::Seek(secs) => seek(&manager, &target, secs),
            Command::SetVolume(v) => match volume::set_volume(&target, v) {
                Ok(applied) => Reply::Accepted(applied),
                Err(e) => Reply::Error(e.message()),
            },
            Command::SetMute(m) => match volume::set_mute(&target, m) {
                Ok(applied) => Reply::Accepted(applied),
                Err(e) => Reply::Error(e.message()),
            },
            Command::Thumbnail => Reply::Thumbnail(thumbnail(&manager, &target)),
            Command::ListSessions => match list_sessions(&manager) {
                Ok(v) => Reply::Sessions(v),
                Err(e) => Reply::Error(e.message()),
            },
        };
        // 请求方可能已经放弃等待（连接断了），送不出去不算错。
        let _ = reply.send(out);
    }
}

/// manager 建不起来时，把已排队的请求用同一条错误答复掉，而不是让它们卡住。
fn drain_with_error(rx: Receiver<(Command, Sender<Reply>)>, msg: &str) {
    for (_, reply) in rx {
        let _ = reply.send(Reply::Error(msg.to_string()));
    }
}

/// 把 SMTC 初始化失败翻译成能指导操作的中文提示。
fn smtc_error_hint(e: &windows::core::Error) -> String {
    if e.code().0 as u32 == 0x8007_0424 {
        "SMTC 不可用（0x80070424）：当前是非交互会话。\
         cross-next 必须在已登录的桌面会话里运行，不能做成服务或从 SSH 启动。"
            .to_string()
    } else {
        format!("SMTC 初始化失败：{}", e.message())
    }
}

enum Transport {
    Prev,
    Next,
    Toggle,
}

/// 发传输命令。
///
/// 刻意不查 `Controls()` 的能力位：实测 QQ音乐 上报不完整（状态为 Opened 时
/// IsPauseEnabled=false），照能力位禁用按钮会让播放键一直是灰的。直接发命令，
/// 让应用自己拒绝，把 false 如实回传。
fn transport(manager: &SessionManager, target: &str, what: Transport) -> Reply {
    let session = match find_session(manager, target) {
        Ok(Some((s, _))) => Some(s),
        // 没有任何 SMTC 会话 —— 有些 QQ音乐 版本只接了老的 WM_APPCOMMAND 通路。
        // 降级到模拟媒体键，这是唯一还能用的办法。
        Ok(None) => None,
        Err(e) => return Reply::Error(e.message()),
    };

    let Some(session) = session else {
        return media_key_fallback(what);
    };

    let op = match what {
        Transport::Prev => session.TrySkipPreviousAsync(),
        Transport::Next => session.TrySkipNextAsync(),
        Transport::Toggle => session.TryTogglePlayPauseAsync(),
    };

    match op.and_then(block_on) {
        Ok(accepted) => Reply::Accepted(accepted),
        Err(e) => Reply::Error(e.message()),
    }
}

/// SMTC 不可用时发模拟媒体键。
///
/// 注意这是全局按键，不针对 QQ音乐 —— 系统决定投给谁。台式机上如果浏览器
/// 正在放视频，可能被它抢走。所以只在没有 SMTC 会话时才走这条路。
fn media_key_fallback(what: Transport) -> Reply {
    let key = match what {
        Transport::Prev => mediakey::Key::Prev,
        Transport::Next => mediakey::Key::Next,
        Transport::Toggle => mediakey::Key::PlayPause,
    };

    if mediakey::send(key) {
        Reply::Accepted(true)
    } else {
        // SendInput 返回数不符，通常是 UIPI 拦了低权限进程的按键注入。
        Reply::Error(
            "媒体键注入被拒。若 QQ音乐 以管理员身份运行，cross-next 也需要以管理员身份运行。"
                .into(),
        )
    }
}

/// 找目标会话。返回 (会话, 是否命中 target)。
///
/// 先按 AUMID 子串匹配 target；匹配不到就回退到系统当前会话，并把第二个返回值
/// 置 false，让前端能提示"没锁定到 QQ音乐"。
fn find_session(manager: &SessionManager, target: &str) -> Result<Option<(Session, bool)>> {
    let needle = target.to_lowercase();

    for session in manager.GetSessions()? {
        let aumid = session.SourceAppUserModelId()?.to_string().to_lowercase();
        if aumid.contains(&needle) {
            return Ok(Some((session, true)));
        }
    }

    // GetCurrentSession 在无会话时返回 Err 而非 None。
    Ok(manager.GetCurrentSession().ok().map(|s| (s, false)))
}

fn snapshot(manager: &SessionManager, target: &str) -> Result<Snapshot> {
    let Some((session, matched)) = find_session(manager, target)? else {
        // 没有 SMTC 会话。但 QQ音乐 可能仍在播 —— 有些版本只接老的
        // WM_APPCOMMAND 通路。用 Core Audio 判断它是否真的在出声：
        // 有音频会话就说明在跑，此时按键仍然可用，只是读不到元数据。
        let vol = volume::read(target).ok().flatten();
        if let Some(state) = vol {
            return Ok(Snapshot {
                present: true,
                mode: Mode::MediaKey,
                matched_target: true,
                volume: Some(state.level),
                muted: Some(state.muted),
                ..Default::default()
            });
        }
        return Ok(Snapshot::default());
    };

    let mut snap = Snapshot {
        present: true,
        mode: Mode::Smtc,
        matched_target: matched,
        aumid: session.SourceAppUserModelId()?.to_string(),
        ..Default::default()
    };

    // 元数据任一项取不到都不该让整个快照失败，逐项降级。
    if let Ok(props) = session.TryGetMediaPropertiesAsync().and_then(block_on) {
        snap.title = props.Title().map(|s| s.to_string()).unwrap_or_default();
        snap.artist = props.Artist().map(|s| s.to_string()).unwrap_or_default();
        snap.album = props
            .AlbumTitle()
            .map(|s| s.to_string())
            .unwrap_or_default();
    }

    if let Ok(info) = session.GetPlaybackInfo() {
        snap.playing = matches!(info.PlaybackStatus(), Ok(PlaybackStatus::Playing));
        snap.can_seek = info
            .Controls()
            .and_then(|c| c.IsPlaybackPositionEnabled())
            .unwrap_or(false);
    }

    read_timeline(&session, &mut snap);

    // 曲目标识兼作封面缓存键。用元数据而非计数器，这样曲目没变时前端不会重取图。
    snap.art_tag = art_tag(&snap);

    // 音量取不到是正常情况（QQ音乐 未出声时 Core Audio 里没有它的会话），
    // 用 None 表达"暂不可用"，前端置灰滑杆而不是显示 0。
    if let Ok(Some(state)) = volume::read(target) {
        snap.volume = Some(state.level);
        snap.muted = Some(state.muted);
    }

    Ok(snap)
}

/// 读时间轴并把 Position 外推到「现在」。
///
/// `Position` 是**快照**，只在 `LastUpdatedTime` 那一刻准确。播放中必须加上从那一刻
/// 到现在的墙钟差值，否则前端每秒拿到的都是同一个陈旧值，进度条会一格一格地跳。
///
/// 外推刻意放在服务端：浏览器在另一台机器上，它的时钟和本机的 `LastUpdatedTime`
/// 不在同一基准上，拿客户端时钟去算会引入两机时钟偏移。
///
/// 时间单位统一是 100ns —— `TimeSpan::Duration`、`DateTime::UniversalTime`
/// 和 `FILETIME` 都以此计，且后两者同为 1601 纪元，可以直接相减。
fn read_timeline(session: &Session, snap: &mut Snapshot) {
    const TICKS_PER_SEC: f64 = 1e7;

    let Ok(t) = session.GetTimelineProperties() else {
        return;
    };

    let start = t.StartTime().map(|d| d.Duration).unwrap_or(0);
    let end = t.EndTime().map(|d| d.Duration).unwrap_or(0);

    // EndTime 为 0 表示这个会话不上报时间轴（部分 QQ音乐 状态如此）。
    // 此时整块降级为 None，让前端隐藏进度条而不是显示错误数据。
    if end <= start {
        return;
    }

    let Ok(pos) = t.Position().map(|d| d.Duration) else {
        return;
    };

    let mut position = pos;

    // 只在播放中外推。暂停时 Position 就是当前值，加上时间差反而会往前跑。
    // LastUpdatedTime 可能比系统时间略新（或系统时钟刚被调整过），负值当 0 处理，
    // 不要让进度倒退。
    if snap.playing {
        let elapsed = t
            .LastUpdatedTime()
            .map(|d| now_ticks().saturating_sub(d.UniversalTime))
            .unwrap_or(0)
            .max(0);
        position = position.saturating_add(elapsed);
    }

    // 外推可能略微越过终点，夹住以免前端算出 >100%。
    let position = position.clamp(start, end);

    // StartTime 通常是 0，但不保证；一律以它为原点。
    snap.position = Some((position - start) as f64 / TICKS_PER_SEC);
    snap.duration = Some((end - start) as f64 / TICKS_PER_SEC);
}

/// 当前时间，100ns 计，1601 纪元 —— 与 `DateTime::UniversalTime` 同基准。
fn now_ticks() -> i64 {
    use windows::Win32::System::SystemInformation::GetSystemTimeAsFileTime;

    let ft = unsafe { GetSystemTimeAsFileTime() };
    ((ft.dwHighDateTime as i64) << 32) | (ft.dwLowDateTime as i64)
}

/// 拖动到指定位置（秒）。
fn seek(manager: &SessionManager, target: &str, secs: f64) -> Reply {
    let Ok(Some((session, _))) = find_session(manager, target) else {
        // mediakey 模式下没有会话，定位无从下手。
        return Reply::Error("当前通路不支持拖动定位".into());
    };

    // 加上 StartTime 换回绝对位置 —— 前端发来的是相对于起点的秒数。
    let start = session
        .GetTimelineProperties()
        .and_then(|t| t.StartTime())
        .map(|d| d.Duration)
        .unwrap_or(0);
    let ticks = start + (secs.max(0.0) * 1e7) as i64;

    match session
        .TryChangePlaybackPositionAsync(ticks)
        .and_then(block_on)
    {
        Ok(accepted) => Reply::Accepted(accepted),
        Err(e) => Reply::Error(e.message()),
    }
}

/// 由曲目元数据派生一个短标识。前端用它判断曲目是否换了。
fn art_tag(snap: &Snapshot) -> String {
    let joined: Vec<u8> = snap
        .title
        .bytes()
        .chain(snap.artist.bytes())
        .chain(snap.album.bytes())
        .collect();
    fnv(&joined)
}

/// FNV-1a。够用，且不必为此引依赖。
fn fnv(bytes: &[u8]) -> String {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for b in bytes {
        hash ^= *b as u64;
        hash = hash.wrapping_mul(0x100_0000_01b3);
    }
    format!("{hash:016x}")
}

/// 取封面字节和它的内容哈希。
///
/// 刻意不做服务端缓存：SMTC 的缩略图有时比曲目元数据慢一拍，缓存住就再也刷不掉
/// 那张过期的图了。前端只在换歌时取（外加一次延迟核对），一首歌两次读取而已。
fn thumbnail(manager: &SessionManager, target: &str) -> Option<(String, Vec<u8>)> {
    let (session, _) = find_session(manager, target).ok()??;
    let props = block_on(session.TryGetMediaPropertiesAsync().ok()?).ok()?;

    let stream = block_on(props.Thumbnail().ok()?.OpenReadAsync().ok()?).ok()?;
    let size = stream.Size().ok()?.min(MAX_THUMBNAIL as u64) as u32;
    if size == 0 {
        return None;
    }

    // DataReader 比手工操作 Buffer 省事，且能一次读满。
    let buffer = Buffer::Create(size).ok()?;
    let buffer = block_on_progress(
        stream
            .ReadAsync(&buffer, size, InputStreamOptions::ReadAhead)
            .ok()?,
    )
    .ok()?;

    let reader = DataReader::FromBuffer(&buffer).ok()?;
    let len = reader.UnconsumedBufferLength().ok()? as usize;
    let mut bytes = vec![0u8; len];
    reader.ReadBytes(&mut bytes).ok()?;

    Some((fnv(&bytes), bytes))
}

fn list_sessions(manager: &SessionManager) -> Result<Vec<SessionInfo>> {
    let current = manager
        .GetCurrentSession()
        .ok()
        .and_then(|s| s.SourceAppUserModelId().ok())
        .map(|s| s.to_string());

    let mut out = Vec::new();
    for session in manager.GetSessions()? {
        let aumid = session.SourceAppUserModelId()?.to_string();
        let (mut title, mut artist) = (String::new(), String::new());
        if let Ok(props) = session.TryGetMediaPropertiesAsync().and_then(block_on) {
            title = props.Title().map(|s| s.to_string()).unwrap_or_default();
            artist = props.Artist().map(|s| s.to_string()).unwrap_or_default();
        }
        let status = session
            .GetPlaybackInfo()
            .and_then(|i| i.PlaybackStatus())
            .map(|s| format!("{s:?}"))
            .unwrap_or_else(|_| "未知".into());

        out.push(SessionInfo {
            is_current: current.as_deref() == Some(aumid.as_str()),
            aumid,
            title,
            artist,
            status,
        });
    }
    Ok(out)
}
