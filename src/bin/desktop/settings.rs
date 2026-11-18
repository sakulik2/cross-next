//! `desktop.json`：窗口位置、是否置顶、是否接管媒体键、组合键。放在 exe 同目录。
//!
//! 刻意不写进 `remote.json`：那份是 `remote`/`listen` 共用的连接配置，`listen` 会
//! 热重载它，往里写窗口坐标会让它每拖一次窗口就重载一次。这里的东西也与连接无关。

use crate::hotkey::Action;
use cross_next::jsonlite;
use std::path::PathBuf;

#[derive(Clone, Debug, PartialEq)]
pub struct Settings {
    /// 窗口左上角，物理像素。`None` 表示没存过，用默认位置。
    pub pos: Option<(i32, i32)>,
    pub topmost: bool,
    /// 是否接管媒体键（取代 `listen.exe`）。
    pub media_keys: bool,
    /// 与 [`Action::ALL`] 一一对应的组合键文本。
    pub hotkeys: Vec<(Action, String)>,
}

impl Default for Settings {
    fn default() -> Self {
        Settings {
            pos: None,
            // 默认置顶：这个程序存在的理由就是打游戏时用，无边框窗口的游戏会盖住
            // 不置顶的窗口。嫌碍事从右键菜单关掉，或最小化到任务栏。
            topmost: true,
            // 默认接管：小组件就是为了取代「再开一个 listen.exe」。服务端在本机时
            // 会自动不接管（见 `hotkey::is_local`），不用为那种情形改默认值。
            media_keys: true,
            hotkeys: Action::ALL
                .iter()
                .map(|a| (*a, a.default_binding().to_string()))
                .collect(),
        }
    }
}

pub fn path() -> PathBuf {
    std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(|d| d.join("desktop.json")))
        .unwrap_or_else(|| PathBuf::from("desktop.json"))
}

/// 解析。缺的字段取默认值 —— 新版本加了字段，旧文件照样能用。
pub fn parse(text: &str) -> Settings {
    let d = Settings::default();
    let x = jsonlite::number(text, "x").map(|v| v as i32);
    let y = jsonlite::number(text, "y").map(|v| v as i32);
    Settings {
        pos: x.zip(y),
        topmost: jsonlite::field(text, "topmost").map_or(d.topmost, |v| v == "true"),
        media_keys: jsonlite::field(text, "media_keys").map_or(d.media_keys, |v| v == "true"),
        hotkeys: Action::ALL
            .iter()
            .map(|a| {
                let v = jsonlite::string(text, a.key());
                (*a, v.unwrap_or_else(|| a.default_binding().to_string()))
            })
            .collect(),
    }
}

/// 序列化。组合键文本只会是字母数字加号，但仍按 JSON 转义，免得手改出一个引号
/// 就把整份文件写坏。
pub fn render(s: &Settings) -> String {
    let mut out = String::from("{\n");
    if let Some((x, y)) = s.pos {
        out.push_str(&format!("  \"x\": {x},\n  \"y\": {y},\n"));
    }
    out.push_str(&format!("  \"topmost\": {},\n", s.topmost));
    out.push_str(&format!("  \"media_keys\": {},\n", s.media_keys));
    let lines: Vec<String> = s
        .hotkeys
        .iter()
        .map(|(a, v)| format!("  \"{}\": \"{}\"", a.key(), escape(v)))
        .collect();
    out.push_str(&lines.join(",\n"));
    out.push_str("\n}\n");
    out
}

fn escape(s: &str) -> String {
    s.replace('\\', "\\\\").replace('"', "\\\"")
}

/// 读配置。文件不存在时写一份默认的 —— 组合键要改就得知道往哪儿改。
pub fn load() -> Settings {
    match std::fs::read_to_string(path()) {
        Ok(text) => parse(&text),
        Err(_) => {
            let s = Settings::default();
            let _ = std::fs::write(path(), render(&s));
            s
        }
    }
}

/// 存窗口位置与置顶状态。
pub fn save_window(pos: (i32, i32), topmost: bool) {
    update(|s| {
        s.pos = Some(pos);
        s.topmost = topmost;
    });
}

/// 存「接管媒体键」开关。
pub fn save_media_keys(on: bool) {
    update(|s| s.media_keys = on);
}

/// 改一项再写回。其余各项从**磁盘上的文件**重新读，而不是用内存里那份：用户可能
/// 开着程序在改组合键，还没点「重新载入」，此时拖一下窗口不该把这些改动覆盖掉。
fn update(change: impl FnOnce(&mut Settings)) {
    let mut s = std::fs::read_to_string(path())
        .map(|t| parse(&t))
        .unwrap_or_default();
    change(&mut s);
    let _ = std::fs::write(path(), render(&s));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trips() {
        let mut s = Settings {
            pos: Some((-1200, 300)),
            topmost: false,
            media_keys: false,
            ..Default::default()
        };
        s.hotkeys[0].1 = "Win+F9".into();
        assert_eq!(parse(&render(&s)), s);
    }

    #[test]
    fn missing_fields_take_defaults() {
        assert_eq!(parse("{}"), Settings::default());
        assert_eq!(parse("garbage"), Settings::default());
        // 只给一个坐标不算有位置。
        assert_eq!(parse(r#"{"x": 5}"#).pos, None);
    }

    #[test]
    fn empty_binding_is_kept_not_defaulted() {
        // 用户清空某个键表示「不要这个组合键」，不能读回来又变成默认值。
        let s = parse(r#"{"hotkey_next": ""}"#);
        assert_eq!(s.hotkeys[1], (Action::Next, String::new()));
    }

    #[test]
    fn quotes_do_not_corrupt_file() {
        let mut s = Settings::default();
        s.hotkeys[0].1 = "Ctrl+\"".into();
        assert_eq!(parse(&render(&s)).hotkeys[0].1, "Ctrl+\"");
    }
}
