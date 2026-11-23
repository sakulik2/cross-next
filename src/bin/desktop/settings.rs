//! 小组件的设置：是否置顶、是否接管媒体键、组合键。写在 exe 同目录的 `config.json` 里，
//! 与服务端、`remote`/`listen` 共用同一份文件，各读各的字段。
//!
//! 共用的前提是写入时不能整份重写：服务端写 token、小组件写开关，谁整份覆盖谁就会抹掉
//! 别人的字段。所以一律走 `jsonlite::set`，只换自己那一项的值，其余原样保留。
//!
//! 窗口位置不存：每次启动都回到默认位置，拖到哪算哪。

use crate::hotkey::Action;
use cross_next::{client, jsonlite};
use std::path::PathBuf;

#[derive(Clone, Debug, PartialEq)]
pub struct Settings {
    pub topmost: bool,
    /// 是否接管媒体键（取代 `listen.exe`）。
    pub media_keys: bool,
    /// 与 [`Action::ALL`] 一一对应的组合键文本。
    pub hotkeys: Vec<(Action, String)>,
}

impl Default for Settings {
    fn default() -> Self {
        Settings {
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
    client::config_path()
}

/// 解析。缺的字段取默认值 —— 新版本加了字段，旧文件照样能用。
pub fn parse(text: &str) -> Settings {
    let d = Settings::default();
    Settings {
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

/// 每一项的 (键, 写进 JSON 的字面量)。组合键文本只会是字母数字加号，但仍按 JSON
/// 转义，免得手改出一个引号就把整份文件写坏。
fn entries(s: &Settings) -> Vec<(String, String)> {
    let mut out = vec![
        ("topmost".to_string(), s.topmost.to_string()),
        ("media_keys".to_string(), s.media_keys.to_string()),
    ];
    for (a, v) in &s.hotkeys {
        out.push((a.key().to_string(), format!("\"{}\"", escape(v))));
    }
    out
}

fn escape(s: &str) -> String {
    s.replace('\\', "\\\\").replace('"', "\\\"")
}

/// 文件里还没有的项按默认值补上，已有的原样不动 —— 组合键要改就得知道有哪些可改。
fn fill_missing(text: &str) -> String {
    entries(&Settings::default())
        .into_iter()
        .fold(text.to_string(), |t, (k, v)| {
            if jsonlite::field(&t, &k).is_some() {
                t
            } else {
                jsonlite::set(&t, &k, &v)
            }
        })
}

/// 读设置，顺手把旧版 `desktop.json` 并进来（窗口位置不迁移），再把缺的项补写进文件。
pub fn load() -> Settings {
    let keys: Vec<String> = entries(&Settings::default())
        .into_iter()
        .map(|(k, _)| k)
        .collect();
    let keys: Vec<&str> = keys.iter().map(String::as_str).collect();
    client::merge_legacy("desktop.json", &keys);
    let text = std::fs::read_to_string(path()).unwrap_or_default();
    let filled = fill_missing(&text);
    if filled != text {
        let _ = std::fs::write(path(), &filled);
    }
    parse(&filled)
}

/// 存「置顶」开关。
pub fn save_topmost(on: bool) {
    update("topmost", &on.to_string());
}

/// 存「接管媒体键」开关。
pub fn save_media_keys(on: bool) {
    update("media_keys", &on.to_string());
}

/// 改一项再写回。文本从**磁盘上的文件**现读，而不是用内存里那份：用户可能开着程序在改
/// 组合键、服务端也可能刚补写过 token，这里只动自己那一项，别的都不能覆盖。
fn update(key: &str, raw: &str) {
    let text = std::fs::read_to_string(path()).unwrap_or_default();
    let _ = std::fs::write(path(), jsonlite::set(&text, key, raw));
}

#[cfg(test)]
mod tests {
    use super::*;

    fn render(s: &Settings) -> String {
        entries(s)
            .iter()
            .fold("{}".to_string(), |t, (k, v)| jsonlite::set(&t, k, v))
    }

    #[test]
    fn round_trips() {
        let mut s = Settings {
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

    #[test]
    fn fill_missing_keeps_other_programs_fields() {
        let t = r#"{"host": "1.2.3.4", "token": "abc", "media_keys": false}"#;
        let out = fill_missing(t);
        // 别的程序的字段原样在。
        assert_eq!(jsonlite::field(&out, "host").as_deref(), Some("1.2.3.4"));
        assert_eq!(jsonlite::field(&out, "token").as_deref(), Some("abc"));
        // 已有的值不被默认值顶掉，缺的补上。
        assert!(!parse(&out).media_keys);
        assert!(parse(&out).topmost);
        assert!(jsonlite::field(&out, "hotkey_prev").is_some());
        // 补过一次就稳定了。
        assert_eq!(fill_missing(&out), out);
    }
}
