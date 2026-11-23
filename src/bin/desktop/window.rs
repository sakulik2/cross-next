//! 小组件窗口：绘制、鼠标、组合键、右键菜单、任务栏缩略图。
//!
//! 窗口样式是实测过的组合（一次性探针，在本机 Win11 上跑）：
//!
//! - `WS_EX_NOACTIVATE`：点它**不抢前台**。无边框窗口模式的游戏开着时点一下小组件，
//!   游戏不会失去焦点、不会松开鼠标捕获。探针里点击前后前台窗口句柄不变。
//!   系统仍会发一个 `WM_ACTIVATE`，所以 `WM_MOUSEACTIVATE` 再显式答 `MA_NOACTIVATE`。
//! - `WS_EX_APPWINDOW`：NOACTIVATE 窗口默认**不上任务栏**，要靠它强制出现 ——
//!   缩略图按钮挂在任务栏按钮上，没有任务栏按钮就什么都没有。
//! - `WS_MINIMIZEBOX | WS_SYSMENU`：没有标题栏也得带上，点任务栏按钮才会最小化/还原。
//!   探针里连点四次是 激活 → 最小化 → 还原 → 最小化，和普通窗口一致。
//!
//! 拖动自己实现（捕获鼠标 + SetWindowPos），不用 `HTCAPTION`：NOACTIVATE 窗口走
//! 系统的标题栏拖动有已知的怪癖，自己做行为可控。
//!
//! **重入**：`SetWindowPos`、`SetWindowTextW`、`ReleaseCapture`、跨进程的 COM 调用
//! 都会同步地再进一次窗口过程。所以 `App` 放在 `RefCell` 里，每个处理函数都是
//! 「借出来算好要做什么 → 还回去 → 再调这些 API」。借用用 `try_borrow_mut`，
//! 万一漏了某条路径也只是那条消息走默认处理，而不是 panic（release 下 panic 即 abort）。

use crate::hotkey::{self, Action};
use crate::model::{self, Glyph, Link, View};
use crate::pixels::{self, Image, Rgb};
use crate::settings::{self, Settings};
use crate::taskbar::{self, Thumbbar};
use crate::worker::{self, Cmd, Cover, Event, Inbox, WM_APP_EVENTS};
use cross_next::client::{self, Config};
use cross_next::{icon, net, ui};
use std::cell::RefCell;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::mpsc::Sender;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use windows::Win32::Foundation::{COLORREF, HWND, LPARAM, LRESULT, POINT, RECT, WPARAM};
use windows::Win32::Graphics::Dwm::{
    DWMWA_FORCE_ICONIC_REPRESENTATION, DWMWA_HAS_ICONIC_BITMAP, DWMWA_USE_IMMERSIVE_DARK_MODE,
    DWMWA_WINDOW_CORNER_PREFERENCE, DWMWINDOWATTRIBUTE, DwmInvalidateIconicBitmaps,
    DwmSetIconicLivePreviewBitmap, DwmSetIconicThumbnail, DwmSetWindowAttribute,
};
use windows::Win32::Graphics::Gdi::{
    BI_RGB, BITMAPINFO, BITMAPINFOHEADER, BeginPaint, BitBlt, CLEARTYPE_QUALITY,
    CLIP_DEFAULT_PRECIS, CreateCompatibleDC, CreateDIBSection, CreateFontW, DEFAULT_CHARSET,
    DIB_RGB_COLORS, DT_END_ELLIPSIS, DT_LEFT, DT_NOPREFIX, DT_SINGLELINE, DT_VCENTER, DeleteDC,
    DeleteObject, DrawTextW, EndPaint, GdiFlush, HBITMAP, HDC, HFONT, HGDIOBJ, InvalidateRect,
    OUT_DEFAULT_PRECIS, PAINTSTRUCT, SRCCOPY, SelectObject, SetBkMode, SetTextColor, TRANSPARENT,
};
use windows::Win32::UI::HiDpi::{GetDpiForSystem, GetDpiForWindow};
use windows::Win32::UI::Input::KeyboardAndMouse::{
    HOT_KEY_MODIFIERS, MOD_NOREPEAT, RegisterHotKey, ReleaseCapture, SetCapture, TME_LEAVE,
    TRACKMOUSEEVENT, TrackMouseEvent, UnregisterHotKey,
};
use windows::Win32::UI::Shell::{ShellExecuteW, THBN_CLICKED};
use windows::Win32::UI::WindowsAndMessaging::{
    AppendMenuW, CreatePopupMenu, CreateWindowExW, DefWindowProcW, DestroyMenu, DestroyWindow,
    DispatchMessageW, GetCursorPos, GetMessageW, GetSystemMetrics, GetWindowRect, HMENU,
    HWND_NOTOPMOST, HWND_TOPMOST, IDC_ARROW, IDC_HAND, KillTimer, LoadCursorW, MA_NOACTIVATE,
    MF_CHECKED, MF_GRAYED, MF_POPUP, MF_SEPARATOR, MF_STRING, MSG, PostMessageW, PostQuitMessage,
    RegisterClassExW, RegisterWindowMessageW, SM_CXICON, SM_CXSMICON, SPI_GETWORKAREA, SW_MINIMIZE,
    SW_SHOWNOACTIVATE, SW_SHOWNORMAL, SWP_NOACTIVATE, SWP_NOMOVE, SWP_NOSIZE, SWP_NOZORDER,
    SYSTEM_PARAMETERS_INFO_UPDATE_FLAGS, SetCursor, SetForegroundWindow, SetTimer, SetWindowPos,
    SetWindowTextW, ShowWindow, SystemParametersInfoW, TPM_RETURNCMD, TPM_RIGHTBUTTON,
    TrackPopupMenu, TranslateMessage, WM_CAPTURECHANGED, WM_COMMAND, WM_DESTROY, WM_DPICHANGED,
    WM_DWMSENDICONICLIVEPREVIEWBITMAP, WM_DWMSENDICONICTHUMBNAIL, WM_ERASEBKGND, WM_HOTKEY,
    WM_LBUTTONDOWN, WM_LBUTTONUP, WM_MOUSEACTIVATE, WM_MOUSEMOVE, WM_MOUSEWHEEL, WM_NULL, WM_PAINT,
    WM_RBUTTONUP, WM_SETCURSOR, WM_SETTINGCHANGE, WM_TIMER, WNDCLASSEXW, WS_EX_APPWINDOW,
    WS_EX_NOACTIVATE, WS_EX_TOPMOST, WS_MINIMIZEBOX, WS_POPUP, WS_SYSMENU,
};
use windows::core::{HSTRING, PCWSTR, w};

pub const CLASS: PCWSTR = w!("CrossNextDesktop");

const APP_ICO: &[u8] = include_bytes!("../../../assets/tray.ico");
const PREV_ICO: &[u8] = include_bytes!("../../../assets/prev.ico");
const PLAY_ICO: &[u8] = include_bytes!("../../../assets/play.ico");
const PAUSE_ICO: &[u8] = include_bytes!("../../../assets/pause.ico");
const NEXT_ICO: &[u8] = include_bytes!("../../../assets/next.ico");

// ---- 版式，单位是 DIP（96 DPI 下的像素），用时按 DPI 换算 ----

const W: i32 = 344;
const H: i32 = 100;
const PAD: i32 = 12;
const COVER: i32 = 76;
const COVER_RADIUS: i32 = 8;
const TEXT_X: i32 = PAD + COVER + 14;
const TEXT_R: i32 = W - 14;
/// 按钮的圆形点击区直径。
const BTN: i32 = 34;
const BTN_Y: i32 = 70;
const BTN_GAP: i32 = 46;
const GLYPH: i32 = 20;
/// 右上角关闭按钮：点击区直径、X 的边长、距窗口上沿和右沿的空隙。
const CLOSE: i32 = 22;
const CLOSE_GLYPH: i32 = 10;
const CLOSE_INSET: i32 = 6;
/// 标题行的右沿：比文字区窄一截，给关闭按钮让位。
const TITLE_R: i32 = W - CLOSE_INSET - CLOSE - 4;
const PLAY_GLYPH: i32 = 24;
/// 进度条贴着窗口底边，横贯全宽。
const BAR_H: i32 = 3;
/// 没封面时占位图上的程序图标。
const LOGO: i32 = 36;

// ---- 配色，与网页一致 ----

const FG: Rgb = [0xf4, 0xf4, 0xf7];
const FG_DIM: Rgb = [0xa8, 0xa8, 0xb4];
const DEFAULT_A: Rgb = [0x2a, 0x2a, 0x35];
const DEFAULT_B: Rgb = [0x16, 0x16, 0x1c];

/// 乐观更新的宽限期：点了播放/暂停后这段时间里不信服务端的播放状态，
/// 否则 QQ音乐 还没来得及切换，按钮会闪回去。同网页的 1.2 秒。
const OPTIMISTIC: Duration = Duration::from_millis(1200);
/// 一次性提示停留多久。
const FLASH: Duration = Duration::from_millis(1800);
/// 音量每档（滚轮一格、组合键一下），百分点。
const VOLUME_STEP: i32 = 5;

const TIMER_TICK: usize = 1;

/// windows crate 把它放在 `Win32_UI_Controls` 里，为一个常量多开一个 feature 不值。
const WM_MOUSELEAVE: u32 = 0x02A3;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Btn {
    Prev,
    PlayPause,
    Next,
    Close,
}

impl Btn {
    const ALL: [Btn; 4] = [Btn::Prev, Btn::PlayPause, Btn::Next, Btn::Close];

    /// 关闭不对应任何播放动作，由窗口自己处理。
    fn action(self) -> Option<Action> {
        match self {
            Btn::Prev => Some(Action::Prev),
            Btn::PlayPause => Some(Action::PlayPause),
            Btn::Next => Some(Action::Next),
            Btn::Close => None,
        }
    }

    /// 播放按钮在连不上服务端时停用；关闭按钮不能跟着停，连不上时更需要它。
    fn needs_server(self) -> bool {
        self != Btn::Close
    }

    /// 圆形点击区的直径，DIP。
    fn diameter(self) -> i32 {
        if self == Btn::Close { CLOSE } else { BTN }
    }
}

/// 按 DPI 换算后的版式。
#[derive(Clone, Copy)]
struct Layout {
    dpi: u32,
}

impl Layout {
    fn px(self, v: i32) -> i32 {
        (v * self.dpi as i32 + 48) / 96
    }

    fn size(self) -> (i32, i32) {
        (self.px(W), self.px(H))
    }

    fn button_center(self, b: Btn) -> (i32, i32) {
        let off = match b {
            Btn::Prev => -BTN_GAP,
            Btn::PlayPause => 0,
            Btn::Next => BTN_GAP,
            Btn::Close => {
                let r = CLOSE / 2;
                return (self.px(W - CLOSE_INSET - r), self.px(CLOSE_INSET + r));
            }
        };
        let cx = (TEXT_X + TEXT_R) / 2;
        (self.px(cx + off), self.px(BTN_Y))
    }

    fn hit(self, x: i32, y: i32) -> Option<Btn> {
        Btn::ALL.into_iter().find(|b| {
            let r = self.px(b.diameter()) / 2;
            let (cx, cy) = self.button_center(*b);
            (x - cx).pow(2) + (y - cy).pow(2) <= r * r
        })
    }
}

/// 一个组合键的注册结果，菜单里要列出来。
#[derive(Clone)]
struct HotkeyStatus {
    action: Action,
    text: String,
    /// `Err` 是给人看的原因：被占用，或写法不对。
    result: Result<(), String>,
}

/// 媒体键的热键 id 起点。组合键占 1..=6，另起一段免得撞号。
const MEDIA_ID: i32 = 101;
/// 媒体键被占时隔多久再试一次。占用方退出后就能自动拿回来，不用重启小组件。
const MEDIA_RETRY: Duration = Duration::from_secs(5);

/// 媒体键为什么没（全部）拿在手里。菜单里要照实说。
#[derive(Clone, Copy, PartialEq, Eq)]
enum MediaWhy {
    Held,
    /// 设置里关了。
    Off,
    /// 服务端就在本机，接管会死循环（见 `hotkey::is_local`）。
    Local,
    /// 服务端连不上，暂时放开，让本机播放器接管 —— 同 `listen.exe` 的让出。
    Yielded,
    /// 想拿，但有的键被别的程序占着。
    Busy,
}

/// 接管媒体键，取代 `listen.exe`。
struct MediaKeys {
    enabled: bool,
    /// 与 `hotkey::MEDIA_KEYS` 一一对应。
    held: [bool; 3],
    why: MediaWhy,
    /// (host, 是否本机)。枚举网卡不便宜，`config.json` 热重载换了 host 才重算。
    local: Option<(String, bool)>,
    /// 上次注册失败的时间，用于 `MEDIA_RETRY`。
    failed_at: Option<Instant>,
}

/// 随 DPI 变化的绘制资源。DPI 一变整个丢掉重建。
struct Res {
    dpi: u32,
    title: HFONT,
    sub: HFONT,
    prev: Image,
    play: Image,
    pause: Image,
    next: Image,
    disc: Image,
    /// 关闭按钮：X 与它悬停时的底（比播放按钮的小）。
    close: Image,
    close_disc: Image,
    placeholder: Image,
    /// 缩放过的封面，以 `Arc` 指针识别是哪一张。
    cover: Option<(usize, Image)>,
}

impl Res {
    fn new(dpi: u32) -> Res {
        let l = Layout { dpi };
        let glyph = |ico: &[u8], dip| {
            let size = l.px(dip);
            icon::pick_at_least(ico, size)
                .and_then(icon::decode_bgra)
                .map(|(side, px)| {
                    Image {
                        w: side,
                        h: side,
                        px,
                    }
                    .resize(size as usize, size as usize)
                })
                .unwrap_or_else(|| Image::new(1, 1))
        };

        // 占位：一块淡白圆角方块，中间放程序图标。
        let cover = l.px(COVER) as usize;
        let mut placeholder = Image::new(cover, cover);
        let mut plate = pixels::disc(cover, FG);
        plate = {
            // disc 是圆的；占位要的是圆角方块，所以重新铺满再按封面的圆角裁。
            for p in plate.px.as_chunks_mut::<4>().0.iter_mut() {
                p.copy_from_slice(&[FG[2], FG[1], FG[0], 255]);
            }
            plate.round_corners(l.px(COVER_RADIUS) as f32);
            plate
        };
        placeholder.blend(&plate, 0, 0, 22);
        let logo = glyph(APP_ICO, LOGO);
        let off = ((cover - logo.w) / 2) as i32;
        placeholder.blend(&logo, off, off, 200);

        Res {
            dpi,
            title: font(l.px(15), 600),
            sub: font(l.px(12), 400),
            prev: glyph(PREV_ICO, GLYPH),
            play: glyph(PLAY_ICO, PLAY_GLYPH),
            pause: glyph(PAUSE_ICO, PLAY_GLYPH),
            next: glyph(NEXT_ICO, GLYPH),
            disc: pixels::disc(l.px(BTN) as usize, FG),
            close: pixels::cross(l.px(CLOSE_GLYPH) as usize, l.px(3) as f32 / 2.0, FG),
            close_disc: pixels::disc(l.px(CLOSE) as usize, FG),
            placeholder,
            cover: None,
        }
    }

    fn cover(&mut self, c: &Arc<Cover>, size: usize) -> &Image {
        let key = Arc::as_ptr(c) as usize;
        if self
            .cover
            .as_ref()
            .is_none_or(|(k, img)| *k != key || img.w != size)
        {
            let mut img = c.img.resize(size, size);
            img.round_corners(Layout { dpi: self.dpi }.px(COVER_RADIUS) as f32);
            self.cover = Some((key, img));
        }
        &self.cover.as_ref().unwrap().1
    }
}

impl Drop for Res {
    fn drop(&mut self) {
        unsafe {
            let _ = DeleteObject(self.title.into());
            let _ = DeleteObject(self.sub.into());
        }
    }
}

fn font(px: i32, weight: i32) -> HFONT {
    unsafe {
        CreateFontW(
            -px,
            0,
            0,
            0,
            weight,
            0,
            0,
            0,
            DEFAULT_CHARSET,
            OUT_DEFAULT_PRECIS,
            CLIP_DEFAULT_PRECIS,
            CLEARTYPE_QUALITY,
            0,
            w!("Microsoft YaHei UI"),
        )
    }
}

struct App {
    hwnd: HWND,
    config: Arc<Mutex<Config>>,
    tx: Sender<Cmd>,
    inbox: Inbox,

    link: Link,
    /// 收到 `link` 的时刻，进度条据此外推。
    received: Instant,
    cover: Option<Arc<Cover>>,

    /// 乐观更新：点了播放/暂停后先显示这个，直到期限。
    pending: Option<(Glyph, Instant)>,
    /// 第二行临时换成这句话，直到期限。
    flash: Option<(String, Instant)>,
    /// 连续调音量时以它为基准往上加，不等服务端回报 —— 否则滚得快时每格都从
    /// 旧值算起，提示里的数字会来回跳。
    volume_preview: Option<f64>,
    /// 触控板会发不足一格（120）的滚动量，攒够一格再算。
    wheel: i32,

    hover: Option<Btn>,
    pressed: Option<Btn>,
    /// 拖动中：(按下时的光标位置, 按下时的窗口左上角)。
    drag: Option<(POINT, POINT)>,
    tracking: bool,

    topmost: bool,
    hotkeys: Vec<HotkeyStatus>,
    media: MediaKeys,
    res: Option<Res>,
    /// 上次设到窗口标题上的文字，没变就不再设。
    caption: String,
    /// 封面或标题变了，任务栏缩略图需要重新要图。
    iconic_dirty: bool,
}

impl App {
    fn layout(&self) -> Layout {
        Layout {
            dpi: unsafe { GetDpiForWindow(self.hwnd) }.max(96),
        }
    }

    fn addr(&self) -> String {
        let c = self.config.lock().unwrap();
        format!("{}:{}", c.host, c.port)
    }

    /// 当前该显示的内容：服务端状态，叠上乐观更新与临时提示。
    fn view(&self) -> (View, Option<f64>) {
        let now = Instant::now();
        let mut v = model::view(&self.link, &self.addr());
        if let Some((g, until)) = self.pending
            && now < until
            && v.enabled
        {
            v.glyph = g;
        }
        if let Some((text, until)) = &self.flash
            && now < *until
        {
            v.line2 = text.clone();
        }
        let progress = match &self.link {
            Link::Ok(s) => model::progress(s, self.received.elapsed().as_secs_f64()),
            _ => None,
        };
        (v, progress)
    }

    fn state(&self) -> Option<&model::State> {
        match &self.link {
            Link::Ok(s) => Some(s),
            _ => None,
        }
    }

    fn notice(&mut self, text: impl Into<String>, long: bool) {
        let d = if long { FLASH * 4 } else { FLASH };
        self.flash = Some((text.into(), Instant::now() + d));
    }

    /// 执行一个操作：发给网络线程，同时做乐观更新。
    fn act(&mut self, action: Action) {
        let cmd = match action {
            Action::Prev => Cmd::Transport("prev"),
            Action::Next => Cmd::Transport("next"),
            Action::PlayPause => {
                let (v, _) = self.view();
                let flipped = match v.glyph {
                    Glyph::Play => Glyph::Pause,
                    Glyph::Pause => Glyph::Play,
                };
                self.pending = Some((flipped, Instant::now() + OPTIMISTIC));
                Cmd::Transport("playpause")
            }
            Action::VolumeUp | Action::VolumeDown => {
                let d = if action == Action::VolumeUp {
                    VOLUME_STEP
                } else {
                    -VOLUME_STEP
                };
                // 明知 QQ音乐 没有音频会话就不发了：服务端只会回一句被截断的长报错。
                // 说法与网页的音量提示一致。
                if self.state().is_some_and(|s| s.volume.is_none()) {
                    self.notice("音量暂不可用（QQ音乐 未输出声音）", false);
                    return;
                }
                self.nudge_volume(d);
                Cmd::Volume(d)
            }
            Action::Mute => {
                if let Some(m) = self.state().and_then(|s| s.muted) {
                    self.notice(if m { "取消静音" } else { "已静音" }, false);
                }
                Cmd::Mute
            }
        };
        let _ = self.tx.send(cmd);
    }

    /// 音量的乐观显示。还没拿到状态时没有基准，不猜，等服务端回报。
    fn nudge_volume(&mut self, d: i32) {
        let flashing = self
            .flash
            .as_ref()
            .is_some_and(|(_, t)| Instant::now() < *t);
        let base = self
            .volume_preview
            .filter(|_| flashing)
            .or_else(|| self.state().and_then(|s| s.volume));
        if let Some(base) = base {
            let v = (base + d as f64 / 100.0).clamp(0.0, 1.0);
            self.volume_preview = Some(v);
            self.notice(format!("音量 {}%", (v * 100.0).round()), false);
        }
    }

    /// 收取网络线程攒下的事件。
    fn drain(&mut self) {
        let events = std::mem::take(&mut *self.inbox.lock().unwrap());
        for e in events {
            match e {
                Event::Link(l) => {
                    self.link = l;
                    self.received = Instant::now();
                }
                Event::Cover(c) => {
                    self.cover = c;
                    self.iconic_dirty = true;
                }
                Event::Notice(n) => self.notice(n, false),
            }
        }
    }

    /// 合成整个画面（不含文字）。
    fn canvas(&mut self, view: &View, progress: Option<f64>) -> Image {
        let l = self.layout();
        if self.res.as_ref().is_none_or(|r| r.dpi != l.dpi) {
            self.res = Some(Res::new(l.dpi));
        }
        let (w, h) = l.size();
        let mut img = Image::new(w as usize, h as usize);

        let cover = self.cover.clone().filter(|_| view.art);
        let (a, b) = cover
            .as_ref()
            .and_then(|c| c.colors)
            .unwrap_or((DEFAULT_A, DEFAULT_B));
        let center = (l.px(PAD) + l.px(COVER) / 2) as f32;
        img.glow(a, b, center, center, l.px(260) as f32);

        let res = self.res.as_mut().unwrap();
        match &cover {
            Some(c) => img.blend(
                res.cover(c, l.px(COVER) as usize),
                l.px(PAD),
                l.px(PAD),
                255,
            ),
            None => img.blend(&res.placeholder, l.px(PAD), l.px(PAD), 255),
        }

        for b in Btn::ALL {
            let (cx, cy) = l.button_center(b);
            let live = view.enabled || !b.needs_server();
            if live && self.hover == Some(b) {
                let alpha = if self.pressed == Some(b) { 64 } else { 36 };
                let disc = if b == Btn::Close {
                    &res.close_disc
                } else {
                    &res.disc
                };
                let r = (disc.w / 2) as i32;
                img.blend(disc, cx - r, cy - r, alpha);
            }
            let glyph = match (b, view.glyph) {
                (Btn::Close, _) => &res.close,
                (Btn::Prev, _) => &res.prev,
                (Btn::Next, _) => &res.next,
                (Btn::PlayPause, Glyph::Play) => &res.play,
                (Btn::PlayPause, Glyph::Pause) => &res.pause,
            };
            let half = (glyph.w / 2) as i32;
            img.blend(
                glyph,
                cx - half,
                cy - half,
                match (b, live) {
                    (Btn::Close, _) if self.hover == Some(b) => 255,
                    // 关闭按钮平时半透明，不抢眼。
                    (Btn::Close, _) => 130,
                    (_, true) => 240,
                    (_, false) => 80,
                },
            );
        }

        if let Some(p) = progress {
            let bar = l.px(BAR_H).max(2);
            img.fill(0, h - bar, w as usize, bar as usize, FG, 34);
            let filled = (w as f64 * p).round() as usize;
            img.fill(0, h - bar, filled, bar as usize, FG, 220);
        }
        img
    }

    /// 画出完整的一帧（含文字），返回位图。调用方负责 `DeleteObject`。
    fn render(&mut self) -> Option<HBITMAP> {
        let (view, progress) = self.view();
        let canvas = self.canvas(&view, progress);
        let l = self.layout();
        let res = self.res.as_ref()?;
        let line1 = RECT {
            left: l.px(TEXT_X),
            top: l.px(13),
            right: l.px(TITLE_R),
            bottom: l.px(34),
        };
        let line2 = RECT {
            left: l.px(TEXT_X),
            top: l.px(34),
            right: l.px(TEXT_R),
            bottom: l.px(52),
        };
        let flashing = self
            .flash
            .as_ref()
            .is_some_and(|(_, t)| Instant::now() < *t);
        let texts = [
            (res.title, FG, view.line1.as_str(), line1),
            (
                res.sub,
                if flashing { FG } else { FG_DIM },
                view.line2.as_str(),
                line2,
            ),
        ];
        bitmap(&canvas, |dc| {
            for (font, color, text, rect) in texts {
                draw_text(dc, font, color, text, rect);
            }
        })
    }

    /// 任务栏缩略图：`side`×`side` 的方图，有封面就是封面本身；没有封面时在封面色
    /// 的底上放程序图标。
    ///
    /// 预览框的比例跟着交的图走：DWM 给的上限在 Win11 上固定是 400×235（与窗口形状
    /// 无关），交一张 235×235 的图，预览就是方的，不会被拉伸。
    fn thumbnail(&mut self, side: usize) -> Image {
        let (view, _) = self.view();
        let cover = self.cover.clone().filter(|_| view.art);
        if let Some(c) = &cover {
            return c.img.resize(side, side);
        }
        let mut img = Image::new(side, side);
        let mid = side as f32 / 2.0;
        img.glow(DEFAULT_A, DEFAULT_B, mid, mid, side as f32 * 0.7);
        let size = (side / 2).max(16);
        if let Some(logo) = icon::pick_at_least(APP_ICO, size as i32)
            .and_then(icon::decode_bgra)
            .map(|(s, px)| Image { w: s, h: s, px }.resize(size, size))
        {
            let at = (side as i32 - logo.w as i32) / 2;
            img.blend(&logo, at, at, 255);
        }
        img
    }
}

thread_local! {
    static APP: RefCell<Option<App>> = const { RefCell::new(None) };
    /// 单独放：它的方法是跨进程 COM 调用，会在等待期间泵消息、重入窗口过程，
    /// 和 APP 分开借就不会互相卡住。
    static BAR: RefCell<Option<Thumbbar>> = const { RefCell::new(None) };
}

/// `TaskbarButtonCreated` 的消息号，运行时才有。
static TASKBAR_BUTTON_CREATED: AtomicU32 = AtomicU32::new(0);

/// 借出 App 做点事。借不到（重入中）就返回 None，调用方走默认处理。
fn with<R>(f: impl FnOnce(&mut App) -> R) -> Option<R> {
    APP.with(|a| a.try_borrow_mut().ok()?.as_mut().map(f))
}

fn with_bar(f: impl FnOnce(&mut Thumbbar)) {
    BAR.with(|b| {
        if let Ok(mut b) = b.try_borrow_mut()
            && let Some(bar) = b.as_mut()
        {
            f(bar);
        }
    });
}

/// 已有实例时把它叫出来，返回是否找到。重复启动不该开出第二个小组件。
pub fn activate_existing() -> bool {
    use windows::Win32::UI::WindowsAndMessaging::{FindWindowW, IsIconic, SW_RESTORE};
    let Ok(h) = (unsafe { FindWindowW(CLASS, None) }) else {
        return false;
    };
    if h.is_invalid() {
        return false;
    }
    unsafe {
        let cmd = if IsIconic(h).as_bool() {
            SW_RESTORE
        } else {
            SW_SHOWNOACTIVATE
        };
        let _ = ShowWindow(h, cmd);
    }
    true
}

/// 关掉正在运行的实例，返回是否真的关了一个。给 `--stop` 与更新脚本用，
/// 做法同 `tray::stop_running`：发 `WM_CLOSE` 走正常退出（会放开组合键），轮询到窗口消失。
pub fn stop_running() -> bool {
    use windows::Win32::UI::WindowsAndMessaging::{FindWindowW, WM_CLOSE};
    let found = |()| {
        unsafe { FindWindowW(CLASS, None) }
            .ok()
            .filter(|h| !h.is_invalid())
    };
    let Some(h) = found(()) else { return false };
    if unsafe { PostMessageW(Some(h), WM_CLOSE, WPARAM(0), LPARAM(0)) }.is_err() {
        return false;
    }
    for _ in 0..100 {
        std::thread::sleep(Duration::from_millis(20));
        if found(()).is_none() {
            break;
        }
    }
    true
}

/// 建窗口、起网络线程、跑消息循环。窗口关闭时返回。
pub fn run(config: Config, settings: Settings) -> Result<(), String> {
    TASKBAR_BUTTON_CREATED.store(
        unsafe { RegisterWindowMessageW(w!("TaskbarButtonCreated")) },
        Ordering::Relaxed,
    );

    let hwnd = create(&settings).ok_or("无法创建窗口")?;
    let config = Arc::new(Mutex::new(config));
    let (tx, inbox) = worker::spawn(hwnd, Arc::clone(&config));
    let hotkeys = register_hotkeys(hwnd, &settings);

    let mut app = App {
        hwnd,
        config,
        tx,
        inbox,
        link: Link::Connecting,
        received: Instant::now(),
        cover: None,
        pending: None,
        flash: None,
        volume_preview: None,
        wheel: 0,
        hover: None,
        pressed: None,
        drag: None,
        tracking: false,
        topmost: settings.topmost,
        hotkeys,
        media: MediaKeys {
            enabled: settings.media_keys,
            held: [false; 3],
            why: MediaWhy::Off,
            local: None,
            failed_at: None,
        },
        res: None,
        caption: String::new(),
        iconic_dirty: true,
    };
    if let Some(msg) = hotkey_problems(&app.hotkeys) {
        app.notice(msg, true);
    }
    APP.with(|a| *a.borrow_mut() = Some(app));
    BAR.with(|b| *b.borrow_mut() = Some(Thumbbar::new()));
    sync_media(hwnd, true);

    unsafe {
        SetTimer(Some(hwnd), TIMER_TICK, 500, None);
        let _ = ShowWindow(hwnd, SW_SHOWNOACTIVATE);
        let mut msg = MSG::default();
        while GetMessageW(&mut msg, None, 0, 0).as_bool() {
            let _ = TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
    }
    Ok(())
}

fn create(settings: &Settings) -> Option<HWND> {
    unsafe {
        let big = icon::load(APP_ICO, GetSystemMetrics(SM_CXICON).max(32));
        let small = icon::load(APP_ICO, GetSystemMetrics(SM_CXSMICON).max(16));
        let wc = WNDCLASSEXW {
            cbSize: std::mem::size_of::<WNDCLASSEXW>() as u32,
            lpfnWndProc: Some(wndproc),
            lpszClassName: CLASS,
            hIcon: big.unwrap_or_default(),
            hIconSm: small.unwrap_or_default(),
            hCursor: LoadCursorW(None, IDC_ARROW).unwrap_or_default(),
            ..Default::default()
        };
        if RegisterClassExW(&wc) == 0 {
            return None;
        }

        let mut ex = WS_EX_NOACTIVATE | WS_EX_APPWINDOW;
        if settings.topmost {
            ex |= WS_EX_TOPMOST;
        }
        let (x, y) = initial_position();
        let hwnd = CreateWindowExW(
            ex,
            CLASS,
            w!("cross-next"),
            WS_POPUP | WS_MINIMIZEBOX | WS_SYSMENU,
            x,
            y,
            1,
            1,
            None,
            None,
            None,
            None,
        )
        .ok()?;

        // 尺寸要按窗口所在显示器的 DPI 算，所以先建在目标位置，再定大小。
        let (w, h) = Layout {
            dpi: GetDpiForWindow(hwnd).max(96),
        }
        .size();
        let _ = SetWindowPos(
            hwnd,
            None,
            0,
            0,
            w,
            h,
            SWP_NOMOVE | SWP_NOZORDER | SWP_NOACTIVATE,
        );

        let on = 1i32;
        let set = |attr: DWMWINDOWATTRIBUTE, v: &i32| {
            let _ = DwmSetWindowAttribute(hwnd, attr, v as *const i32 as *const _, 4);
        };
        // 任务栏缩略图与 Peek 用我们给的图（封面），而不是窗口截图 ——
        // 最小化之后系统手里没有截图，只会显示一个图标。
        set(DWMWA_FORCE_ICONIC_REPRESENTATION, &on);
        set(DWMWA_HAS_ICONIC_BITMAP, &on);
        // Win11 圆角（DWMWCP_ROUND）与深色边框。旧系统不认这两个属性，忽略失败即可。
        set(DWMWA_WINDOW_CORNER_PREFERENCE, &2);
        set(DWMWA_USE_IMMERSIVE_DARK_MODE, &on);
        Some(hwnd)
    }
}

/// 主屏工作区右下角。位置不持久化，每次启动都从这里开始，拖到哪算哪。
fn initial_position() -> (i32, i32) {
    let mut work = RECT::default();
    let _ = unsafe {
        SystemParametersInfoW(
            SPI_GETWORKAREA,
            0,
            Some(&mut work as *mut RECT as *mut _),
            SYSTEM_PARAMETERS_INFO_UPDATE_FLAGS(0),
        )
    };
    let l = Layout {
        dpi: unsafe { GetDpiForSystem() }.max(96),
    };
    let (w, h) = l.size();
    (work.right - w - l.px(24), work.bottom - h - l.px(24))
}

/// 注册全部组合键。重新载入时也走这里，所以先注销旧的。
fn register_hotkeys(hwnd: HWND, settings: &Settings) -> Vec<HotkeyStatus> {
    settings
        .hotkeys
        .iter()
        .enumerate()
        .map(|(i, (action, text))| {
            let id = i as i32 + 1;
            let _ = unsafe { UnregisterHotKey(Some(hwnd), id) };
            let result = match hotkey::parse(text) {
                Ok(None) => Ok(()),
                Ok(Some(k)) => {
                    let mut mods = HOT_KEY_MODIFIERS(k.mods);
                    if !action.repeats() {
                        mods |= MOD_NOREPEAT;
                    }
                    // 失败只说明被占了 —— Win32 没有查占用者的 API（见 listen.rs）。
                    unsafe { RegisterHotKey(Some(hwnd), id, mods, k.vk) }
                        .map_err(|_| "被其它程序占用".to_string())
                }
                Err(e) => Err(e),
            };
            HotkeyStatus {
                action: *action,
                text: text.clone(),
                result,
            }
        })
        .collect()
}

/// 让媒体键的持有状态跟上设置与连接状况。连接每次变化、每个节拍都会调，没变化时
/// 什么也不做。`force` 跳过被占时的重试间隔。
///
/// 要拿键时 `listen.exe` 若在跑，先把它关掉：两者抢的是同三个键，同时开着只有一个
/// 拿得到，而小组件就是来取代它的。
fn sync_media(hwnd: HWND, force: bool) {
    enum Step {
        Release(Vec<usize>),
        Take(Vec<usize>),
    }
    let step = with(|app| {
        let host = app.config.lock().unwrap().host.clone();
        let local = match &app.media.local {
            Some((h, l)) if *h == host => *l,
            _ => {
                let l = hotkey::is_local(&host, &net::private_ipv4s());
                app.media.local = Some((host, l));
                l
            }
        };
        let m = &mut app.media;
        let why = if !m.enabled {
            Some(MediaWhy::Off)
        } else if local {
            Some(MediaWhy::Local)
        } else if matches!(app.link, Link::Unreachable) {
            Some(MediaWhy::Yielded)
        } else {
            None
        };
        if let Some(why) = why {
            m.why = why;
            m.failed_at = None;
            let held: Vec<usize> = (0..3).filter(|&i| m.held[i]).collect();
            m.held = [false; 3];
            return (!held.is_empty()).then_some(Step::Release(held));
        }
        let missing: Vec<usize> = (0..3).filter(|&i| !m.held[i]).collect();
        if missing.is_empty() {
            m.why = MediaWhy::Held;
            return None;
        }
        if !force && m.failed_at.is_some_and(|t| t.elapsed() < MEDIA_RETRY) {
            return None;
        }
        Some(Step::Take(missing))
    });

    match step {
        Some(Some(Step::Release(ids))) => {
            for i in ids {
                let _ = unsafe { UnregisterHotKey(Some(hwnd), MEDIA_ID + i as i32) };
            }
        }
        Some(Some(Step::Take(missing))) => {
            // 等它退出、放开热键再注册，最多 1 秒。没在跑时只是一次 FindWindowW。
            let closed = ui::close_instance(ui::LISTEN_MARKER, Duration::from_secs(1));
            let got: Vec<(usize, bool)> = missing
                .into_iter()
                .map(|i| {
                    let (vk, ..) = hotkey::MEDIA_KEYS[i];
                    // 裸键，不带修饰；按住不连发，免得一按跳好几首。
                    let id = MEDIA_ID + i as i32;
                    (
                        i,
                        unsafe { RegisterHotKey(Some(hwnd), id, MOD_NOREPEAT, vk) }.is_ok(),
                    )
                })
                .collect();
            let notify = with(|app| {
                let m = &mut app.media;
                for (i, ok) in got {
                    m.held[i] |= ok;
                }
                let all = m.held.iter().all(|h| *h);
                let first_failure = !all && m.why != MediaWhy::Busy;
                m.why = if all { MediaWhy::Held } else { MediaWhy::Busy };
                m.failed_at = (!all).then(Instant::now);
                if closed {
                    app.notice("已关闭 listen.exe，媒体键改由小组件接管", true);
                } else if first_failure {
                    app.notice("媒体键被其它程序占用，右键菜单里看详情", true);
                }
                closed || first_failure
            });
            if notify == Some(true) {
                refresh(hwnd);
            }
        }
        _ => {}
    }
}

/// 右键菜单里「接管媒体键」那一项的文字。
fn media_label(m: &MediaKeys) -> String {
    let base = "接管媒体键（⏮ ⏯ ⏭）";
    match m.why {
        MediaWhy::Held | MediaWhy::Off => base.into(),
        MediaWhy::Local => format!("{base}\t服务端在本机，不需要"),
        MediaWhy::Yielded => format!("{base}\t服务端连不上，暂时放开"),
        MediaWhy::Busy => {
            let busy: Vec<&str> = hotkey::MEDIA_KEYS
                .iter()
                .zip(m.held)
                .filter(|(_, held)| !held)
                .map(|((_, _, name), _)| *name)
                .collect();
            format!("{base}\t{} 被其它程序占用", busy.join("、"))
        }
    }
}

/// 有组合键没注册上时的一句话提示；全部正常返回 None。
fn hotkey_problems(list: &[HotkeyStatus]) -> Option<String> {
    let bad: Vec<&str> = list
        .iter()
        .filter(|h| h.result.is_err())
        .map(|h| h.text.as_str())
        .collect();
    (!bad.is_empty()).then(|| format!("组合键 {} 不可用，右键菜单里看原因", bad.join("、")))
}

// ---- 绘制辅助 ----

/// 把画布拷进一张 DIB，交给 `draw` 在上面写字，再把 alpha 补成不透明。
fn bitmap(canvas: &Image, draw: impl FnOnce(HDC)) -> Option<HBITMAP> {
    unsafe {
        let info = BITMAPINFO {
            bmiHeader: BITMAPINFOHEADER {
                biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
                biWidth: canvas.w as i32,
                // 负高度 = 自上而下，与 Image 的行序一致。
                biHeight: -(canvas.h as i32),
                biPlanes: 1,
                biBitCount: 32,
                biCompression: BI_RGB.0,
                ..Default::default()
            },
            ..Default::default()
        };
        let mut bits = std::ptr::null_mut();
        let bmp = CreateDIBSection(None, &info, DIB_RGB_COLORS, &mut bits, None, 0).ok()?;
        if bits.is_null() {
            let _ = DeleteObject(bmp.into());
            return None;
        }
        let len = canvas.px.len();
        let buf = std::slice::from_raw_parts_mut(bits as *mut u8, len);
        buf.copy_from_slice(&canvas.px);

        let dc = CreateCompatibleDC(None);
        let old = SelectObject(dc, bmp.into());
        draw(dc);
        // GDI 是批处理的，直接碰位图内存之前必须先冲刷。
        let _ = GdiFlush();
        SelectObject(dc, old);
        let _ = DeleteDC(dc);

        // GDI 写字会把字形像素的 alpha 清零；交给 DWM 的图里那就是一个个透明的洞。
        for p in buf.as_chunks_mut::<4>().0.iter_mut() {
            p[3] = 255;
        }
        Some(bmp)
    }
}

fn draw_text(dc: HDC, font: HFONT, color: Rgb, text: &str, mut rect: RECT) {
    let mut wide: Vec<u16> = text.encode_utf16().collect();
    unsafe {
        SelectObject(dc, font.into());
        SetBkMode(dc, TRANSPARENT);
        SetTextColor(
            dc,
            COLORREF(color[0] as u32 | (color[1] as u32) << 8 | (color[2] as u32) << 16),
        );
        DrawTextW(
            dc,
            &mut wide,
            &mut rect,
            DT_SINGLELINE | DT_END_ELLIPSIS | DT_NOPREFIX | DT_VCENTER | DT_LEFT,
        );
    }
}

fn delete(bmp: HBITMAP) {
    let _ = unsafe { DeleteObject(HGDIOBJ::from(bmp)) };
}

// ---- 状态变化后的收尾 ----

/// 状态变了：重画，并把变化同步到窗口标题、任务栏按钮、缩略图。
/// 这些调用都会重入窗口过程，所以放在借用之外。
fn refresh(hwnd: HWND) {
    let Some((view, caption, iconic)) = with(|app| {
        let (view, _) = app.view();
        let caption = (view.caption != app.caption).then(|| {
            app.caption = view.caption.clone();
            view.caption.clone()
        });
        let iconic = std::mem::take(&mut app.iconic_dirty) || caption.is_some();
        (view, caption, iconic)
    }) else {
        return;
    };

    unsafe {
        if let Some(c) = &caption {
            let _ = SetWindowTextW(hwnd, &HSTRING::from(c.as_str()));
        }
        with_bar(|bar| {
            if let Some(c) = &caption {
                bar.tooltip(hwnd, c);
            }
            bar.update(hwnd, view.glyph, view.enabled);
        });
        if iconic {
            let _ = DwmInvalidateIconicBitmaps(hwnd);
        }
        let _ = InvalidateRect(Some(hwnd), None, false);
    }
}

fn act(hwnd: HWND, action: Action) {
    with(|app| app.act(action));
    refresh(hwnd);
}

// ---- 右键菜单 ----

const MENU_TOPMOST: usize = 1;
const MENU_BROWSER: usize = 2;
const MENU_MINIMIZE: usize = 3;
const MENU_EDIT_KEYS: usize = 4;
const MENU_RELOAD_KEYS: usize = 5;
const MENU_QUIT: usize = 6;
const MENU_MEDIA: usize = 7;

fn show_menu(hwnd: HWND) {
    let Some((topmost, hotkeys, media, media_on)) = with(|app| {
        (
            app.topmost,
            app.hotkeys.clone(),
            media_label(&app.media),
            app.media.enabled,
        )
    }) else {
        return;
    };

    let chosen = unsafe {
        let Ok(menu) = CreatePopupMenu() else { return };
        let Ok(keys) = CreatePopupMenu() else {
            let _ = DestroyMenu(menu);
            return;
        };
        let item = |m: HMENU, flags, id, text: &str| {
            let _ = AppendMenuW(m, flags, id, &HSTRING::from(text));
        };

        for h in &hotkeys {
            let binding = if h.text.trim().is_empty() {
                "未设置".to_string()
            } else {
                h.text.clone()
            };
            let text = match &h.result {
                Ok(()) => format!("{}\t{binding}", h.action.label()),
                Err(e) => format!("{}\t{binding}（{e}）", h.action.label()),
            };
            item(keys, MF_STRING | MF_GRAYED, 0, &text);
        }
        let _ = AppendMenuW(keys, MF_SEPARATOR, 0, PCWSTR::null());
        item(keys, MF_STRING, MENU_EDIT_KEYS, "编辑 config.json");
        item(keys, MF_STRING, MENU_RELOAD_KEYS, "重新载入");

        let check = if topmost { MF_CHECKED } else { MF_STRING };
        item(menu, MF_STRING | check, MENU_TOPMOST, "置顶");
        item(menu, MF_STRING, MENU_MINIMIZE, "最小化到任务栏");
        item(menu, MF_STRING, MENU_BROWSER, "在浏览器中打开");
        item(menu, MF_POPUP, keys.0 as usize, "组合键");
        let check = if media_on { MF_CHECKED } else { MF_STRING };
        item(menu, MF_STRING | check, MENU_MEDIA, &media);
        let _ = AppendMenuW(menu, MF_SEPARATOR, 0, PCWSTR::null());
        item(menu, MF_STRING, MENU_QUIT, "退出");

        let mut pt = POINT::default();
        let _ = GetCursorPos(&mut pt);
        // TrackPopupMenu 要求调用方是前台窗口，否则点别处菜单不消失。这会让游戏
        // 暂时失去焦点 —— 右键菜单是有意的操作，可以接受；左键点按钮则不会。
        let _ = SetForegroundWindow(hwnd);
        let chosen = TrackPopupMenu(
            menu,
            TPM_RETURNCMD | TPM_RIGHTBUTTON,
            pt.x,
            pt.y,
            None,
            hwnd,
            None,
        );
        // 菜单的老毛病：不补这一下，菜单消失后第一次点击会被吞掉。
        let _ = PostMessageW(Some(hwnd), WM_NULL, WPARAM(0), LPARAM(0));
        // 销毁父菜单会连带销毁挂在它上面的子菜单。
        let _ = DestroyMenu(menu);
        chosen.0 as usize
    };

    match chosen {
        MENU_TOPMOST => {
            let Some(on) = with(|app| {
                app.topmost = !app.topmost;
                app.topmost
            }) else {
                return;
            };
            let after = if on { HWND_TOPMOST } else { HWND_NOTOPMOST };
            unsafe {
                let _ = SetWindowPos(
                    hwnd,
                    Some(after),
                    0,
                    0,
                    0,
                    0,
                    SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE,
                );
            }
            settings::save_topmost(on);
        }
        MENU_MINIMIZE => unsafe {
            let _ = ShowWindow(hwnd, SW_MINIMIZE);
        },
        MENU_BROWSER => {
            let Some(url) = with(|app| client::web_url(&app.config.lock().unwrap())) else {
                return;
            };
            unsafe {
                ShellExecuteW(
                    None,
                    w!("open"),
                    &HSTRING::from(url),
                    PCWSTR::null(),
                    PCWSTR::null(),
                    SW_SHOWNORMAL,
                );
            }
        }
        MENU_EDIT_KEYS => unsafe {
            // 显式用记事本：.json 的默认打开方式可能是浏览器或什么都没有。
            let path = HSTRING::from(settings::path().as_os_str());
            ShellExecuteW(
                None,
                w!("open"),
                w!("notepad.exe"),
                &path,
                PCWSTR::null(),
                SW_SHOWNORMAL,
            );
        },
        MENU_RELOAD_KEYS => {
            let s = settings::load();
            let list = register_hotkeys(hwnd, &s);
            with(|app| {
                let msg = hotkey_problems(&list).unwrap_or_else(|| "组合键已重新载入".into());
                app.notice(msg, true);
                app.hotkeys = list;
                app.media.enabled = s.media_keys;
            });
            sync_media(hwnd, true);
            refresh(hwnd);
        }
        MENU_MEDIA => {
            let Some(on) = with(|app| {
                app.media.enabled = !app.media.enabled;
                app.media.enabled
            }) else {
                return;
            };
            settings::save_media_keys(on);
            sync_media(hwnd, true);
        }
        MENU_QUIT => unsafe {
            let _ = DestroyWindow(hwnd);
        },
        _ => {}
    }
}

// ---- 窗口过程 ----

fn loword(v: usize) -> i32 {
    (v & 0xffff) as u16 as i16 as i32
}

fn hiword(v: usize) -> i32 {
    ((v >> 16) & 0xffff) as u16 as i16 as i32
}

unsafe extern "system" fn wndproc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    // 注册消息的消息号在运行时才定，进不了 match 的模式位置。
    if msg != 0 && msg == TASKBAR_BUTTON_CREATED.load(Ordering::Relaxed) {
        let view = with(|app| app.view().0);
        if let Some(v) = view {
            with_bar(|bar| {
                bar.attach(hwnd, v.glyph, v.enabled);
                bar.tooltip(hwnd, &v.caption);
            });
        }
        return LRESULT(0);
    }

    let handled = match msg {
        WM_APP_EVENTS => {
            with(|app| app.drain());
            // 连接状况可能变了：连不上要放开媒体键，恢复了要拿回来。
            sync_media(hwnd, false);
            refresh(hwnd);
            Some(LRESULT(0))
        }

        WM_PAINT => {
            let mut ps = PAINTSTRUCT::default();
            unsafe {
                let hdc = BeginPaint(hwnd, &mut ps);
                if let Some(Some(bmp)) = with(|app| app.render()) {
                    let mem = CreateCompatibleDC(Some(hdc));
                    let old = SelectObject(mem, bmp.into());
                    let mut r = RECT::default();
                    let _ = GetWindowRect(hwnd, &mut r);
                    let _ = BitBlt(
                        hdc,
                        0,
                        0,
                        r.right - r.left,
                        r.bottom - r.top,
                        Some(mem),
                        0,
                        0,
                        SRCCOPY,
                    );
                    SelectObject(mem, old);
                    let _ = DeleteDC(mem);
                    delete(bmp);
                }
                let _ = EndPaint(hwnd, &ps);
            }
            Some(LRESULT(0))
        }

        // 整个客户区每次都整张重画，擦背景只会带来闪烁。
        WM_ERASEBKGND => Some(LRESULT(1)),

        WM_TIMER if wparam.0 == TIMER_TICK => {
            // 媒体键被占时的定时重试。
            sync_media(hwnd, false);
            // 进度条外推、提示与乐观更新到期，都靠这个节拍重画。
            let need = with(|app| {
                let now = Instant::now();
                let expired = app.flash.as_ref().is_some_and(|(_, t)| now >= *t)
                    || app.pending.is_some_and(|(_, t)| now >= t);
                if app.flash.as_ref().is_some_and(|(_, t)| now >= *t) {
                    app.flash = None;
                    app.volume_preview = None;
                }
                if app.pending.is_some_and(|(_, t)| now >= t) {
                    app.pending = None;
                }
                let moving = app.state().is_some_and(|s| s.playing);
                (expired, moving)
            });
            match need {
                Some((true, _)) => refresh(hwnd),
                Some((false, true)) => unsafe {
                    let _ = InvalidateRect(Some(hwnd), None, false);
                },
                _ => {}
            }
            Some(LRESULT(0))
        }

        // 点击不激活：见模块头注释。
        WM_MOUSEACTIVATE => Some(LRESULT(MA_NOACTIVATE as isize)),

        WM_SETCURSOR => {
            let over = with(|app| {
                app.hover
                    .is_some_and(|b| !b.needs_server() || app.view().0.enabled)
            })
            .unwrap_or(false);
            if over {
                unsafe {
                    SetCursor(LoadCursorW(None, IDC_HAND).ok());
                }
                Some(LRESULT(1))
            } else {
                None
            }
        }

        WM_MOUSEMOVE => {
            let (x, y) = (loword(lparam.0 as usize), hiword(lparam.0 as usize));
            let r = with(|app| {
                if let Some((cur0, win0)) = app.drag {
                    let mut cur = POINT::default();
                    let _ = unsafe { GetCursorPos(&mut cur) };
                    return (
                        Some((win0.x + cur.x - cur0.x, win0.y + cur.y - cur0.y)),
                        false,
                        false,
                    );
                }
                let hover = app.layout().hit(x, y);
                let changed = hover != app.hover;
                app.hover = hover;
                let track = !std::mem::replace(&mut app.tracking, true);
                (None, changed, track)
            });
            match r {
                Some((Some((nx, ny)), ..)) => unsafe {
                    let _ = SetWindowPos(
                        hwnd,
                        None,
                        nx,
                        ny,
                        0,
                        0,
                        SWP_NOSIZE | SWP_NOZORDER | SWP_NOACTIVATE,
                    );
                },
                Some((None, changed, track)) => unsafe {
                    if track {
                        // 要 WM_MOUSELEAVE 才知道光标离开了，好撤掉悬停效果。
                        let mut t = TRACKMOUSEEVENT {
                            cbSize: std::mem::size_of::<TRACKMOUSEEVENT>() as u32,
                            dwFlags: TME_LEAVE,
                            hwndTrack: hwnd,
                            dwHoverTime: 0,
                        };
                        let _ = TrackMouseEvent(&mut t);
                    }
                    if changed {
                        let _ = InvalidateRect(Some(hwnd), None, false);
                    }
                },
                None => {}
            }
            Some(LRESULT(0))
        }

        WM_MOUSELEAVE => {
            with(|app| {
                app.tracking = false;
                app.hover = None;
            });
            unsafe {
                let _ = InvalidateRect(Some(hwnd), None, false);
            }
            Some(LRESULT(0))
        }

        WM_LBUTTONDOWN => {
            let (x, y) = (loword(lparam.0 as usize), hiword(lparam.0 as usize));
            with(|app| {
                let enabled = app.view().0.enabled;
                let hit = app
                    .layout()
                    .hit(x, y)
                    .filter(|b| enabled || !b.needs_server());
                if hit.is_some() {
                    app.pressed = hit;
                } else {
                    // 按钮以外的地方都能拖。
                    let mut cur = POINT::default();
                    let mut r = RECT::default();
                    unsafe {
                        let _ = GetCursorPos(&mut cur);
                        let _ = GetWindowRect(hwnd, &mut r);
                    }
                    app.drag = Some((
                        cur,
                        POINT {
                            x: r.left,
                            y: r.top,
                        },
                    ));
                }
            });
            unsafe {
                SetCapture(hwnd);
                let _ = InvalidateRect(Some(hwnd), None, false);
            }
            Some(LRESULT(0))
        }

        WM_LBUTTONUP => {
            let (x, y) = (loword(lparam.0 as usize), hiword(lparam.0 as usize));
            let r = with(|app| {
                let pressed = app.pressed.take();
                app.drag = None;
                // 按下和松开在同一个按钮上才算一次点击，和系统按钮一样。
                pressed.filter(|b| app.layout().hit(x, y) == Some(*b))
            });
            let _ = unsafe { ReleaseCapture() };
            if let Some(Some(b)) = r {
                match b.action() {
                    Some(a) => act(hwnd, a),
                    // 关闭和右键菜单「退出」走同一条路：DestroyWindow 触发 WM_DESTROY，
                    // 在那里放开热键再退出。
                    None => unsafe {
                        let _ = DestroyWindow(hwnd);
                    },
                }
            }
            unsafe {
                let _ = InvalidateRect(Some(hwnd), None, false);
            }
            Some(LRESULT(0))
        }

        WM_CAPTURECHANGED => {
            with(|app| {
                app.pressed = None;
                app.drag = None;
            });
            Some(LRESULT(0))
        }

        WM_MOUSEWHEEL => {
            let delta = hiword(wparam.0);
            let steps = with(|app| {
                app.wheel += delta;
                let n = app.wheel / 120;
                app.wheel %= 120;
                n
            })
            .unwrap_or(0);
            for _ in 0..steps.abs() {
                act(
                    hwnd,
                    if steps > 0 {
                        Action::VolumeUp
                    } else {
                        Action::VolumeDown
                    },
                );
            }
            Some(LRESULT(0))
        }

        WM_RBUTTONUP => {
            show_menu(hwnd);
            Some(LRESULT(0))
        }

        WM_HOTKEY => {
            let id = wparam.0;
            if let Some((_, a, _)) = id
                .checked_sub(MEDIA_ID as usize)
                .and_then(|i| hotkey::MEDIA_KEYS.get(i))
            {
                act(hwnd, *a);
            } else if let Some(a) = id.checked_sub(1).and_then(|i| Action::ALL.get(i)) {
                act(hwnd, *a);
            }
            Some(LRESULT(0))
        }

        // 任务栏缩略图按钮：通知码在高 16 位，按钮 id 在低 16 位。
        WM_COMMAND if hiword(wparam.0) as u32 == THBN_CLICKED => {
            let b = match loword(wparam.0) as u32 {
                taskbar::ID_PREV => Some(Action::Prev),
                taskbar::ID_PLAYPAUSE => Some(Action::PlayPause),
                taskbar::ID_NEXT => Some(Action::Next),
                _ => None,
            };
            if let Some(a) = b {
                act(hwnd, a);
            }
            Some(LRESULT(0))
        }

        WM_DWMSENDICONICTHUMBNAIL => {
            // lParam：高 16 位最大宽，低 16 位最大高。给的图不能超出。
            let (mw, mh) = (hiword(lparam.0 as usize), loword(lparam.0 as usize));
            // 交边长取短边的方图。DWM 按图本身的比例显示，不会拉伸到上限的比例。
            let side = mw.min(mh).max(1) as usize;
            if let Some(img) = with(|app| app.thumbnail(side))
                && let Some(bmp) = bitmap(&img, |_| {})
            {
                unsafe {
                    let _ = DwmSetIconicThumbnail(hwnd, bmp, 0);
                }
                delete(bmp);
            }
            Some(LRESULT(0))
        }

        WM_DWMSENDICONICLIVEPREVIEWBITMAP => {
            // Peek（悬停缩略图时桌面上显示的那一帧）：给小组件本身的样子。
            if let Some(Some(bmp)) = with(|app| app.render()) {
                unsafe {
                    let _ = DwmSetIconicLivePreviewBitmap(hwnd, bmp, None, 0);
                }
                delete(bmp);
            }
            Some(LRESULT(0))
        }

        // 系统主题切换（深浅色）时广播，lParam 指向 "ImmersiveColorSet"。
        // 缩略图按钮的图标要跟着换色，见 taskbar.rs 模块头注释。
        WM_SETTINGCHANGE if lparam.0 != 0 => {
            let what = unsafe { PCWSTR(lparam.0 as *const u16).to_string() }.unwrap_or_default();
            if what == "ImmersiveColorSet"
                && let Some(v) = with(|app| app.view().0)
            {
                with_bar(|bar| bar.retheme(hwnd, v.glyph, v.enabled));
            }
            None
        }

        WM_DPICHANGED => {
            // 拖到缩放比例不同的显示器上：按系统建议的矩形换尺寸，资源下次绘制时重建。
            let r = unsafe { *(lparam.0 as *const RECT) };
            unsafe {
                let _ = SetWindowPos(
                    hwnd,
                    None,
                    r.left,
                    r.top,
                    r.right - r.left,
                    r.bottom - r.top,
                    SWP_NOZORDER | SWP_NOACTIVATE,
                );
                let _ = InvalidateRect(Some(hwnd), None, false);
            }
            Some(LRESULT(0))
        }

        WM_DESTROY => {
            unsafe {
                let _ = KillTimer(Some(hwnd), TIMER_TICK);
                for id in 1..=Action::ALL.len() as i32 {
                    let _ = UnregisterHotKey(Some(hwnd), id);
                }
                for i in 0..hotkey::MEDIA_KEYS.len() as i32 {
                    let _ = UnregisterHotKey(Some(hwnd), MEDIA_ID + i);
                }
                PostQuitMessage(0);
            }
            Some(LRESULT(0))
        }

        _ => None,
    };

    handled.unwrap_or_else(|| unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn layout_scales_and_hits() {
        let l = Layout { dpi: 96 };
        assert_eq!(l.size(), (W, H));
        let (cx, cy) = l.button_center(Btn::PlayPause);
        assert_eq!(l.hit(cx, cy), Some(Btn::PlayPause));
        assert_eq!(l.hit(0, 0), None, "左上角是封面，不是按钮");
        let (px, py) = l.button_center(Btn::Prev);
        assert_eq!(l.hit(px, py), Some(Btn::Prev));

        let big = Layout { dpi: 192 };
        assert_eq!(big.size(), (W * 2, H * 2));
        let (cx, cy) = big.button_center(Btn::Next);
        assert_eq!(big.hit(cx, cy), Some(Btn::Next));
    }

    #[test]
    fn close_button_hits_and_clears_title() {
        for dpi in [96, 144, 192] {
            let l = Layout { dpi };
            let (cx, cy) = l.button_center(Btn::Close);
            assert_eq!(l.hit(cx, cy), Some(Btn::Close));
            let (w, _) = l.size();
            assert!(cx + l.px(CLOSE) / 2 <= w, "关闭按钮不能超出窗口右沿");
            assert!(
                l.px(TITLE_R) <= cx - l.px(CLOSE) / 2,
                "标题不能压到关闭按钮"
            );
            for b in [Btn::Prev, Btn::PlayPause, Btn::Next] {
                let (bx, by) = l.button_center(b);
                assert_ne!(l.hit(bx, by), Some(Btn::Close));
            }
        }
    }

    #[test]
    fn buttons_do_not_overlap() {
        let l = Layout { dpi: 96 };
        let (a, _) = l.button_center(Btn::Prev);
        let (b, _) = l.button_center(Btn::PlayPause);
        assert!(b - a >= BTN, "相邻按钮的点击区不能重叠");
        let (n, _) = l.button_center(Btn::Next);
        assert!(l.px(TEXT_R) >= n + l.px(BTN) / 2, "按钮不能超出文字区");
    }
}
