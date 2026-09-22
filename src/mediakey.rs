//! 模拟媒体键：不依赖 SMTC 的备用控制通路。
//!
//! 为什么需要它：媒体键有两条独立通路 —— 现代的 SMTC，和老的
//! `WM_APPCOMMAND` / shell hook（XP 时代就有）。有些播放器只接了后者，
//! 那样键盘媒体键照样能用，但 `GetSessions()` 里一个会话都没有。
//! 这时候 SMTC 那条路完全不通，只能走这里。
//!
//! 两个必须知道的取舍：
//!
//!   1. **这是全局的，不针对某个应用。** 系统把按键投给"当前该收媒体键的
//!      那个应用"，具体是谁由系统决定。台式机上如果浏览器正在放视频，
//!      可能被它抢走。SMTC 那条路是定向的，所以能用 SMTC 时优先用 SMTC。
//!   2. **拿不到任何状态。** 只能发命令，读不到曲名、封面、播放状态。
//!
//! UIPI 限制：目标进程以管理员权限运行时，普通权限进程注入的按键会被
//! 静默丢弃。那种情况下 cross-next 也得以管理员身份运行。

use windows::Win32::UI::Input::KeyboardAndMouse::{
    INPUT, INPUT_0, INPUT_KEYBOARD, KEYBD_EVENT_FLAGS, KEYBDINPUT, KEYEVENTF_EXTENDEDKEY,
    KEYEVENTF_KEYUP, SendInput, VIRTUAL_KEY, VK_MEDIA_NEXT_TRACK, VK_MEDIA_PLAY_PAUSE,
    VK_MEDIA_PREV_TRACK,
};

/// 要发的媒体键。
#[derive(Clone, Copy)]
pub enum Key {
    Next,
    Prev,
    PlayPause,
}

impl Key {
    fn vk(self) -> VIRTUAL_KEY {
        match self {
            Key::Next => VK_MEDIA_NEXT_TRACK,
            Key::Prev => VK_MEDIA_PREV_TRACK,
            Key::PlayPause => VK_MEDIA_PLAY_PAUSE,
        }
    }
}

/// 发一次按下+抬起。返回是否成功注入。
///
/// 按下和抬起一起提交：SendInput 保证同一批次的事件不会被其它输入插入打断。
/// 只发按下不发抬起会让系统认为键一直按着，后续按键行为会异常。
pub fn send(key: Key) -> bool {
    let vk = key.vk();

    // 媒体键是扩展键，必须带 KEYEVENTF_EXTENDEDKEY，否则部分应用收不到。
    let down = INPUT {
        r#type: INPUT_KEYBOARD,
        Anonymous: INPUT_0 {
            ki: KEYBDINPUT {
                wVk: vk,
                wScan: 0,
                dwFlags: KEYEVENTF_EXTENDEDKEY,
                time: 0,
                dwExtraInfo: 0,
            },
        },
    };
    let up = INPUT {
        r#type: INPUT_KEYBOARD,
        Anonymous: INPUT_0 {
            ki: KEYBDINPUT {
                wVk: vk,
                wScan: 0,
                dwFlags: KEYBD_EVENT_FLAGS(KEYEVENTF_EXTENDEDKEY.0 | KEYEVENTF_KEYUP.0),
                time: 0,
                dwExtraInfo: 0,
            },
        },
    };

    let inputs = [down, up];
    let sent = unsafe { SendInput(&inputs, std::mem::size_of::<INPUT>() as i32) };

    // 返回实际注入的事件数。被 UIPI 拦掉时会小于请求数。
    sent as usize == inputs.len()
}
