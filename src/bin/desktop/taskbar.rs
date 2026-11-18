//! 任务栏缩略图按钮：鼠标悬停在任务栏图标上时，预览图下方的 ⏮ ⏯ ⏭。
//!
//! 两个时序要求：
//!
//! - `ThumbBarAddButtons` 只能在收到 `TaskbarButtonCreated` 之后调用，早了任务栏
//!   还没有这个按钮，调用失败。
//! - 资源管理器重启后会**再发一次**这个消息，旧的按钮随旧任务栏一起没了，要重新加。
//!   和托盘的 `TaskbarCreated` 是同一回事，不处理的话按钮永久消失。
//!
//! 每个窗口只能 Add 一次（之后只能 Update），所以重新加的时候连 `ITaskbarList3`
//! 一起重建，对应新的任务栏。
//!
//! 图标要跟着**系统主题**换色：缩略图浮窗在浅色主题下是浅灰底，白色字形落上去
//! 几乎看不见（实测，第一版就是这样）。字形 .ico 是白色的，浅色主题下运行时染成
//! 深色；切换主题时系统广播 `WM_SETTINGCHANGE`，由窗口过程调 [`Thumbbar::retheme`]。

use crate::model::Glyph;
use crate::pixels::Image;
use cross_next::icon;
use windows::Win32::Foundation::HWND;
use windows::Win32::Graphics::Gdi::{
    BI_RGB, BITMAPINFO, BITMAPINFOHEADER, CreateBitmap, CreateDIBSection, DIB_RGB_COLORS,
    DeleteObject,
};
use windows::Win32::System::Com::{CLSCTX_INPROC_SERVER, CoCreateInstance};
use windows::Win32::System::Registry::{HKEY_CURRENT_USER, RRF_RT_REG_DWORD, RegGetValueW};
use windows::Win32::UI::Shell::{
    ITaskbarList3, THB_FLAGS, THB_ICON, THB_TOOLTIP, THBF_DISABLED, THBF_ENABLED, THUMBBUTTON,
    TaskbarList,
};
use windows::Win32::UI::WindowsAndMessaging::{
    CreateIconIndirect, DestroyIcon, GetSystemMetrics, HICON, ICONINFO, SM_CXSMICON,
};
use windows::core::{HSTRING, w};

/// 按钮 id，`WM_COMMAND` 的低 16 位。
pub const ID_PREV: u32 = 1;
pub const ID_PLAYPAUSE: u32 = 2;
pub const ID_NEXT: u32 = 3;

const PREV_ICO: &[u8] = include_bytes!("../../../assets/prev.ico");
const PLAY_ICO: &[u8] = include_bytes!("../../../assets/play.ico");
const PAUSE_ICO: &[u8] = include_bytes!("../../../assets/pause.ico");
const NEXT_ICO: &[u8] = include_bytes!("../../../assets/next.ico");

pub struct Thumbbar {
    list: Option<ITaskbarList3>,
    prev: HICON,
    play: HICON,
    pause: HICON,
    next: HICON,
    /// 当前任务栏上的样子，没变就不发 Update —— 那是一次跨进程 COM 调用。
    shown: Option<(Glyph, bool)>,
}

impl Thumbbar {
    pub fn new() -> Thumbbar {
        let mut bar = Thumbbar {
            list: None,
            prev: HICON::default(),
            play: HICON::default(),
            pause: HICON::default(),
            next: HICON::default(),
            shown: None,
        };
        bar.load_icons();
        bar
    }

    /// 系统主题变了：换一套颜色的图标，并强制下一次 [`update`](Self::update) 真的发出去。
    pub fn retheme(&mut self, hwnd: HWND, glyph: Glyph, enabled: bool) {
        self.load_icons();
        self.shown = None;
        self.update(hwnd, glyph, enabled);
    }

    fn load_icons(&mut self) {
        // 深色主题沿用 .ico 原本的白色；浅色主题染成接近黑的深灰，和系统自己的
        // 图标一个深浅。
        let color = if light_theme() {
            [0x1f, 0x1f, 0x1f]
        } else {
            [0xff, 0xff, 0xff]
        };
        // 缩略图按钮用小图标尺寸，随系统缩放是 16/20/24/32。
        let size = unsafe { GetSystemMetrics(SM_CXSMICON) }.max(16);
        for (slot, ico) in [
            (&mut self.prev, PREV_ICO),
            (&mut self.play, PLAY_ICO),
            (&mut self.pause, PAUSE_ICO),
            (&mut self.next, NEXT_ICO),
        ] {
            let new = tinted(ico, size, color).unwrap_or_default();
            if !slot.is_invalid() {
                let _ = unsafe { DestroyIcon(*slot) };
            }
            *slot = new;
        }
    }

    /// 收到 `TaskbarButtonCreated` 时调用：（重新）把按钮加上去。
    pub fn attach(&mut self, hwnd: HWND, glyph: Glyph, enabled: bool) {
        self.list = None;
        self.shown = None;
        let Ok(list) = (unsafe {
            CoCreateInstance::<_, ITaskbarList3>(&TaskbarList, None, CLSCTX_INPROC_SERVER)
        }) else {
            return;
        };
        if unsafe { list.HrInit() }.is_err() {
            return;
        }
        let buttons = self.buttons(glyph, enabled);
        if unsafe { list.ThumbBarAddButtons(hwnd, &buttons) }.is_ok() {
            self.list = Some(list);
            self.shown = Some((glyph, enabled));
        }
    }

    /// 播放状态或可用性变了时调用。
    pub fn update(&mut self, hwnd: HWND, glyph: Glyph, enabled: bool) {
        let Some(list) = &self.list else { return };
        if self.shown == Some((glyph, enabled)) {
            return;
        }
        let buttons = self.buttons(glyph, enabled);
        if unsafe { list.ThumbBarUpdateButtons(hwnd, &buttons) }.is_ok() {
            self.shown = Some((glyph, enabled));
        }
    }

    /// 悬停在缩略图上时的提示文字。
    pub fn tooltip(&self, hwnd: HWND, text: &str) {
        if let Some(list) = &self.list {
            let _ = unsafe { list.SetThumbnailTooltip(hwnd, &HSTRING::from(text)) };
        }
    }

    fn buttons(&self, glyph: Glyph, enabled: bool) -> [THUMBBUTTON; 3] {
        let (play_icon, play_tip) = match glyph {
            Glyph::Play => (self.play, "播放"),
            Glyph::Pause => (self.pause, "暂停"),
        };
        [
            button(ID_PREV, self.prev, "上一首", enabled),
            button(ID_PLAYPAUSE, play_icon, play_tip, enabled),
            button(ID_NEXT, self.next, "下一首", enabled),
        ]
    }
}

fn button(id: u32, icon: HICON, tip: &str, enabled: bool) -> THUMBBUTTON {
    let mut b = THUMBBUTTON {
        dwMask: THB_ICON | THB_TOOLTIP | THB_FLAGS,
        iId: id,
        hIcon: icon,
        dwFlags: if enabled { THBF_ENABLED } else { THBF_DISABLED },
        ..Default::default()
    };
    let wide: Vec<u16> = tip.encode_utf16().collect();
    let n = wide.len().min(b.szTip.len() - 1);
    b.szTip[..n].copy_from_slice(&wide[..n]);
    b
}

/// 任务栏用的是不是浅色主题。读不到（老系统没有这个值）按深色算 —— Win10 早期
/// 的任务栏只有深色。
fn light_theme() -> bool {
    let mut value = 0u32;
    let mut len = 4u32;
    let err = unsafe {
        RegGetValueW(
            HKEY_CURRENT_USER,
            w!(r"Software\Microsoft\Windows\CurrentVersion\Themes\Personalize"),
            w!("SystemUsesLightTheme"),
            RRF_RT_REG_DWORD,
            None,
            Some(&mut value as *mut u32 as *mut _),
            Some(&mut len),
        )
    };
    err.is_ok() && value != 0
}

/// 把白色字形染成 `rgb`，造出 `size`×`size` 的图标。
///
/// 字形只有 alpha 有信息，所以染色就是「颜色全换成 rgb，alpha 照抄」。
/// `CreateIconIndirect` 的 32 位彩色位图要**直通** alpha（不是预乘），
/// 掩码位图全 0 表示透明度完全由 alpha 决定。
fn tinted(ico: &[u8], size: i32, rgb: [u8; 3]) -> Option<HICON> {
    let (side, px) = icon::pick_at_least(ico, size).and_then(icon::decode_bgra)?;
    let img = Image {
        w: side,
        h: side,
        px,
    }
    .resize(size as usize, size as usize);

    unsafe {
        let info = BITMAPINFO {
            bmiHeader: BITMAPINFOHEADER {
                biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
                biWidth: size,
                biHeight: -size,
                biPlanes: 1,
                biBitCount: 32,
                biCompression: BI_RGB.0,
                ..Default::default()
            },
            ..Default::default()
        };
        let mut bits = std::ptr::null_mut();
        let color = CreateDIBSection(None, &info, DIB_RGB_COLORS, &mut bits, None, 0).ok()?;
        if bits.is_null() {
            let _ = DeleteObject(color.into());
            return None;
        }
        let out = std::slice::from_raw_parts_mut(bits as *mut u8, img.px.len());
        for (d, s) in out
            .as_chunks_mut::<4>()
            .0
            .iter_mut()
            .zip(img.px.as_chunks::<4>().0)
        {
            *d = [rgb[2], rgb[1], rgb[0], s[3]];
        }
        let mask = CreateBitmap(size, size, 1, 1, None);
        let icon = CreateIconIndirect(&ICONINFO {
            fIcon: true.into(),
            hbmMask: mask,
            hbmColor: color,
            ..Default::default()
        });
        // 图标创建时会复制这两张位图，原件要自己删。
        let _ = DeleteObject(mask.into());
        let _ = DeleteObject(color.into());
        icon.ok()
    }
}
