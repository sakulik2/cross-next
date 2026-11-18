//! 全局组合键：`"Ctrl+Alt+Left"` 这样的字符串 ↔ `RegisterHotKey` 的参数。
//!
//! 这是独占全屏游戏里唯一不用切出去就能操作的途径 —— 小组件和任务栏按钮都得先
//! 回到桌面才看得见。没有媒体键的键盘也要能用，所以这套组合键可配置。
//!
//! 媒体键另算（[`MEDIA_KEYS`]）：裸键、不可改键位，与 `listen.exe` 抢的是同三个。

use std::net::{IpAddr, Ipv4Addr};

/// 接管的媒体键与对应操作，与 `listen.exe` 一致。键码就是 `VK_MEDIA_*`。
///
/// 不含音量键：那三个调的是**本机**的系统音量，抢走它们会让鼠标所在这台机器
/// 没法调音量。
pub const MEDIA_KEYS: [(u32, Action, &str); 3] = [
    (0xB0, Action::Next, "下一首"),
    (0xB1, Action::Prev, "上一首"),
    (0xB3, Action::PlayPause, "播放/暂停"),
];

/// 服务端是不是就在本机。是的话不能接管媒体键：服务端在 `mediakey` 模式下自己
/// 就是发媒体键（`SendInput`），注入的按键同样会触发 `RegisterHotKey` —— 我们抢下来
/// 转发回去，它再发一次，无限循环地切歌。何况本机的媒体键本来就能直达 QQ音乐。
///
/// `host` 是主机名（非 IP、非 localhost）时判为不是本机：在 UI 线程上做 DNS 解析
/// 可能卡住，而服务端只绑具体的内网 IP，`remote.json` 里填的通常就是那个 IP。
pub fn is_local(host: &str, own: &[Ipv4Addr]) -> bool {
    let host = host.trim();
    if host.eq_ignore_ascii_case("localhost") {
        return true;
    }
    match host.parse::<IpAddr>() {
        Ok(IpAddr::V4(ip)) => ip.is_loopback() || ip.is_unspecified() || own.contains(&ip),
        Ok(IpAddr::V6(ip)) => ip.is_loopback(),
        Err(_) => false,
    }
}

/// 组合键能触发的操作。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Action {
    Prev,
    Next,
    PlayPause,
    VolumeUp,
    VolumeDown,
    Mute,
}

impl Action {
    pub const ALL: [Action; 6] = [
        Action::Prev,
        Action::Next,
        Action::PlayPause,
        Action::VolumeUp,
        Action::VolumeDown,
        Action::Mute,
    ];

    /// `desktop.json` 里的字段名。
    pub fn key(self) -> &'static str {
        match self {
            Action::Prev => "hotkey_prev",
            Action::Next => "hotkey_next",
            Action::PlayPause => "hotkey_playpause",
            Action::VolumeUp => "hotkey_volume_up",
            Action::VolumeDown => "hotkey_volume_down",
            Action::Mute => "hotkey_mute",
        }
    }

    /// 默认键位。静音默认不绑：误触的代价（突然没声）比其它几个大。
    ///
    /// Ctrl+Alt+方向键在部分老 Intel 核显驱动里是「旋转屏幕」，那种机器上会注册
    /// 失败，菜单里能看到是哪个，改 `desktop.json` 换掉即可。
    pub fn default_binding(self) -> &'static str {
        match self {
            Action::Prev => "Ctrl+Alt+Left",
            Action::Next => "Ctrl+Alt+Right",
            Action::PlayPause => "Ctrl+Alt+Space",
            Action::VolumeUp => "Ctrl+Alt+Up",
            Action::VolumeDown => "Ctrl+Alt+Down",
            Action::Mute => "",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Action::Prev => "上一首",
            Action::Next => "下一首",
            Action::PlayPause => "播放/暂停",
            Action::VolumeUp => "音量 +",
            Action::VolumeDown => "音量 -",
            Action::Mute => "静音",
        }
    }

    /// 按住不放时是否连发。音量要连发（按住一路调），切歌不要（按久了连跳几首）。
    pub fn repeats(self) -> bool {
        matches!(self, Action::VolumeUp | Action::VolumeDown)
    }
}

// RegisterHotKey 的修饰位。与 windows crate 的 MOD_* 同值，写成常量是为了让这个
// 模块保持纯逻辑、可测。
pub const MOD_ALT: u32 = 0x1;
pub const MOD_CONTROL: u32 = 0x2;
pub const MOD_SHIFT: u32 = 0x4;
pub const MOD_WIN: u32 = 0x8;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Hotkey {
    pub mods: u32,
    pub vk: u32,
}

/// 解析组合键。空串表示不绑定，返回 `Ok(None)`。
///
/// 除 F1-F24 外必须带修饰键：裸的字母、空格、方向键会在打字和游戏里被吞掉 ——
/// `RegisterHotKey` 是独占的，抢到之后别的程序就收不到了。
pub fn parse(s: &str) -> Result<Option<Hotkey>, String> {
    let s = s.trim();
    if s.is_empty() {
        return Ok(None);
    }

    let mut mods = 0;
    let mut vk = None;
    for part in s.split('+').map(str::trim) {
        let lower = part.to_ascii_lowercase();
        let m = match lower.as_str() {
            "ctrl" | "control" => MOD_CONTROL,
            "alt" => MOD_ALT,
            "shift" => MOD_SHIFT,
            "win" => MOD_WIN,
            _ => 0,
        };
        if m != 0 {
            mods |= m;
            continue;
        }
        if vk.is_some() {
            return Err(format!("「{s}」里有不止一个主键"));
        }
        vk = Some(key_code(&lower).ok_or_else(|| format!("「{s}」里的「{part}」不是认得的键"))?);
    }

    let vk = vk.ok_or_else(|| format!("「{s}」只有修饰键，缺主键"))?;
    let function_key = (0x70..=0x87).contains(&vk);
    if mods == 0 && !function_key {
        return Err(format!("「{s}」必须带 Ctrl/Alt/Shift/Win（F1-F24 除外）"));
    }
    Ok(Some(Hotkey { mods, vk }))
}

/// 主键名 → 虚拟键码。名字已转小写。
fn key_code(name: &str) -> Option<u32> {
    let named = match name {
        "left" => 0x25,
        "up" => 0x26,
        "right" => 0x27,
        "down" => 0x28,
        "space" => 0x20,
        "pageup" | "pgup" => 0x21,
        "pagedown" | "pgdn" => 0x22,
        "end" => 0x23,
        "home" => 0x24,
        "insert" | "ins" => 0x2D,
        "delete" | "del" => 0x2E,
        "pause" => 0x13,
        _ => 0,
    };
    if named != 0 {
        return Some(named);
    }

    let mut chars = name.chars();
    if let (Some(c), None) = (chars.next(), chars.clone().next()) {
        // 单个字母或数字：VK 码就是大写 ASCII。
        if c.is_ascii_alphanumeric() {
            return Some(c.to_ascii_uppercase() as u32);
        }
    }

    // F1-F24：VK_F1 = 0x70。
    let n: u32 = name.strip_prefix('f')?.parse().ok()?;
    (1..=24).contains(&n).then_some(0x6F + n)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_defaults() {
        for a in Action::ALL {
            let b = a.default_binding();
            if b.is_empty() {
                assert_eq!(parse(b), Ok(None));
            } else {
                assert!(parse(b).unwrap().is_some(), "{b}");
            }
        }
        assert_eq!(
            parse("Ctrl+Alt+Left"),
            Ok(Some(Hotkey {
                mods: MOD_CONTROL | MOD_ALT,
                vk: 0x25
            }))
        );
    }

    #[test]
    fn case_and_spacing_insensitive() {
        assert_eq!(
            parse(" ctrl + SHIFT + p "),
            Ok(Some(Hotkey {
                mods: MOD_CONTROL | MOD_SHIFT,
                vk: b'P' as u32
            }))
        );
        assert_eq!(
            parse("Win+7"),
            Ok(Some(Hotkey {
                mods: MOD_WIN,
                vk: b'7' as u32
            }))
        );
    }

    #[test]
    fn function_keys_may_stand_alone() {
        assert_eq!(parse("F13"), Ok(Some(Hotkey { mods: 0, vk: 0x7C })));
        assert_eq!(
            parse("Alt+F24"),
            Ok(Some(Hotkey {
                mods: MOD_ALT,
                vk: 0x87
            }))
        );
        assert!(parse("F25").is_err());
        assert!(parse("F0").is_err());
    }

    #[test]
    fn local_server_detected() {
        let own = [Ipv4Addr::new(192, 168, 1, 20)];
        assert!(is_local("192.168.1.20", &own));
        assert!(is_local(" 192.168.1.20 ", &own));
        assert!(is_local("127.0.0.1", &own));
        assert!(is_local("LocalHost", &own));
        assert!(is_local("::1", &own));
        assert!(!is_local("192.168.1.21", &own));
        assert!(!is_local("desktop-pc", &own), "主机名不解析，按远端处理");
    }

    #[test]
    fn media_keys_never_include_volume() {
        // 音量键属于本机，抢走就调不了本机音量了。
        for (vk, a, _) in MEDIA_KEYS {
            assert!((0xB0..=0xB3).contains(&vk), "{vk:#x}");
            assert!(!a.repeats(), "{a:?}");
        }
    }

    #[test]
    fn rejects_bare_and_malformed() {
        assert!(parse("Space").is_err(), "裸空格会吞掉所有空格键");
        assert!(parse("Left").is_err());
        assert!(parse("Ctrl+Alt").is_err());
        assert!(parse("Ctrl+A+B").is_err());
        assert!(parse("Ctrl+Hyper").is_err());
        assert!(parse("Ctrl+").is_err());
    }
}
