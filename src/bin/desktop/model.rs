//! 纯逻辑：`/api/state` → 界面该显示什么。
//!
//! 和网页的 `render()` 是同一件事的两份实现，文案刻意对齐网页 —— 两个界面对同一
//! 状态说两套话会让人以为其中一个坏了。拆成纯函数是为了能在 `cargo test` 里把每个
//! 分支走一遍；窗口过程里只剩「把 View 画出来」。

use cross_next::jsonlite;

/// 服务端的控制通路，见 README「两种控制通路」。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum Mode {
    Smtc,
    MediaKey,
    #[default]
    None,
}

/// 从 `/api/state` 里取出的、界面用得到的字段。
#[derive(Clone, Debug, PartialEq, Default)]
pub struct State {
    pub present: bool,
    pub mode: Mode,
    pub playing: bool,
    pub title: String,
    pub artist: String,
    pub album: String,
    /// 曲目元数据哈希，换歌才变 —— 据此决定要不要重取封面。
    pub art_tag: String,
    /// 秒。已由服务端外推到响应时刻。
    pub position: Option<f64>,
    pub duration: Option<f64>,
    /// 0.0-1.0；`None` 表示 QQ音乐 当前没有音频会话，不是 0。
    pub volume: Option<f64>,
    pub muted: Option<bool>,
}

/// 解析 `/api/state` 的响应体。服务端出错时返回的是 `{"error": ...}`，
/// 那种情况给 `Err(错误文本)`。
pub fn parse_state(json: &str) -> Result<State, String> {
    if let Some(e) = jsonlite::string(json, "error") {
        return Err(e);
    }
    let present = jsonlite::field(json, "present").ok_or("服务端返回的状态缺少 present")?;
    let flag = |k| jsonlite::field(json, k).as_deref() == Some("true");
    let text = |k| jsonlite::string(json, k).unwrap_or_default();

    Ok(State {
        present: present == "true",
        mode: match jsonlite::field(json, "mode").as_deref() {
            Some("smtc") => Mode::Smtc,
            Some("mediakey") => Mode::MediaKey,
            _ => Mode::None,
        },
        playing: flag("playing"),
        title: text("title"),
        artist: text("artist"),
        album: text("album"),
        art_tag: text("artTag"),
        position: jsonlite::number(json, "position"),
        duration: jsonlite::number(json, "duration"),
        volume: jsonlite::number(json, "volume"),
        muted: jsonlite::field(json, "muted").and_then(|v| match v.as_str() {
            "true" => Some(true),
            "false" => Some(false),
            _ => None,
        }),
    })
}

/// 和服务端的连接状况。
#[derive(Clone, Debug, PartialEq)]
pub enum Link {
    /// 启动后还没拿到第一个回复。
    Connecting,
    Ok(State),
    /// 传输层失败：对端没开、休眠、网络不通。
    Unreachable,
    /// 401：token 对不上。
    Unauthorized,
    /// 其它：服务端报错、返回了意外的状态码。
    Failed(String),
}

/// 播放键画哪个字形。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Glyph {
    Play,
    Pause,
}

/// 界面要画的全部内容。
#[derive(Clone, Debug, PartialEq)]
pub struct View {
    pub line1: String,
    pub line2: String,
    pub glyph: Glyph,
    /// 三个按钮是否可点。只看连接状态 —— 服务端在没有 SMTC 会话时会自己降级到
    /// 媒体键，所以「QQ音乐 没在放」时按钮照样该能按，按下去就是把它叫醒。
    pub enabled: bool,
    /// 窗口标题，也是任务栏缩略图上方那行字和 Alt-Tab 里的名字。
    pub caption: String,
    /// 封面是否适用。false 时不管手上有没有图都画占位 —— 换到媒体键模式后
    /// 不该还挂着上一首的封面。
    pub art: bool,
}

/// 由连接状况推导界面。`addr` 只用于连不上时告诉人连的是哪里。
pub fn view(link: &Link, addr: &str) -> View {
    let idle = |line1: &str, line2: &str, enabled| View {
        line1: line1.into(),
        line2: line2.into(),
        glyph: Glyph::Play,
        enabled,
        caption: "cross-next".into(),
        art: false,
    };

    let s = match link {
        Link::Connecting => return idle("正在连接…", addr, false),
        Link::Unreachable => return idle("连不上服务端", addr, false),
        Link::Unauthorized => return idle("token 无效", "检查 remote.json 里的 token", false),
        Link::Failed(e) => return idle("服务端出错", first_line(e), false),
        Link::Ok(s) => s,
    };

    let glyph = if s.playing { Glyph::Pause } else { Glyph::Play };

    if !s.present {
        return idle("QQ音乐 未在播放", "打开 QQ音乐 并播放任意歌曲", true);
    }

    // mediakey：QQ音乐 没注册 SMTC，读不到任何元数据。按键仍然有效，所以不该
    // 显示成"未连接"，而是说明能力边界。播放状态来自 Core Audio 是否在出声。
    if s.mode == Mode::MediaKey {
        return View {
            glyph,
            ..idle("QQ音乐 正在播放", "此版本不提供曲目信息", true)
        };
    }

    let title = if s.title.is_empty() {
        "未知曲目"
    } else {
        &s.title
    };
    let line2 = [s.artist.as_str(), s.album.as_str()]
        .into_iter()
        .filter(|x| !x.is_empty())
        .collect::<Vec<_>>()
        .join(" · ");
    let caption = if s.artist.is_empty() {
        title.to_string()
    } else {
        format!("{title} - {}", s.artist)
    };

    View {
        line1: title.into(),
        line2,
        glyph,
        enabled: true,
        caption,
        art: true,
    }
}

/// 进度 0.0-1.0。`elapsed` 是收到这份状态之后过去的秒数 —— 播放中要往前推，
/// 否则一秒一次的轮询之间进度条是静止的。用的是本机的单调时钟差值，不是两台
/// 机器的绝对时间，所以不受时钟偏移影响（同 `read_timeline` 的考虑）。
pub fn progress(s: &State, elapsed: f64) -> Option<f64> {
    let (pos, dur) = (s.position?, s.duration?);
    if dur <= 0.0 || !s.present || s.mode != Mode::Smtc {
        return None;
    }
    let pos = if s.playing { pos + elapsed } else { pos };
    Some((pos / dur).clamp(0.0, 1.0))
}

/// 服务端/客户端的错误文本常带一段换行后的建议，单行显示只要第一行。
fn first_line(s: &str) -> &str {
    s.lines().next().unwrap_or("")
}

#[cfg(test)]
mod tests {
    use super::*;

    const ADDR: &str = "192.168.1.5:8770";

    fn smtc() -> State {
        parse_state(
            r#"{"present":true,"mode":"smtc","matched":true,"playing":true,"aumid":"QQMusic",
            "title":"晴天","artist":"周杰伦","album":"叶惠美","artTag":"abc",
            "position":10.000,"duration":200.000,"canSeek":false,"volume":0.5000,"muted":false}"#,
        )
        .unwrap()
    }

    #[test]
    fn parses_server_state() {
        let s = smtc();
        assert!(s.present && s.playing);
        assert_eq!(s.mode, Mode::Smtc);
        assert_eq!((s.title.as_str(), s.artist.as_str()), ("晴天", "周杰伦"));
        assert_eq!(s.art_tag, "abc");
        assert_eq!(
            (s.position, s.duration, s.volume),
            (Some(10.0), Some(200.0), Some(0.5))
        );
        assert_eq!(s.muted, Some(false));
    }

    #[test]
    fn parses_nulls_as_none() {
        let s = parse_state(
            r#"{"present":false,"mode":"none","playing":false,"title":"","artist":"",
            "album":"","artTag":"","position":null,"duration":null,"volume":null,"muted":null}"#,
        )
        .unwrap();
        assert_eq!(
            (s.position, s.duration, s.volume, s.muted),
            (None, None, None, None)
        );
        assert_eq!(s.mode, Mode::None);
    }

    #[test]
    fn server_error_and_garbage_are_errors() {
        assert_eq!(
            parse_state(r#"{"error":"媒体线程没响应"}"#),
            Err("媒体线程没响应".into())
        );
        assert!(parse_state("<html>").is_err());
    }

    #[test]
    fn titles_with_quotes_survive() {
        let s =
            parse_state(r#"{"present":true,"mode":"smtc","title":"\"Heroes\"","artist":"Bowie"}"#)
                .unwrap();
        assert_eq!(view(&Link::Ok(s), ADDR).caption, "\"Heroes\" - Bowie");
    }

    #[test]
    fn playing_track() {
        let v = view(&Link::Ok(smtc()), ADDR);
        assert_eq!(v.line1, "晴天");
        assert_eq!(v.line2, "周杰伦 · 叶惠美");
        assert_eq!(v.caption, "晴天 - 周杰伦");
        assert_eq!(v.glyph, Glyph::Pause);
        assert!(v.enabled && v.art);
    }

    #[test]
    fn paused_track_shows_play() {
        let s = State {
            playing: false,
            ..smtc()
        };
        assert_eq!(view(&Link::Ok(s), ADDR).glyph, Glyph::Play);
    }

    #[test]
    fn missing_metadata_falls_back() {
        let s = State {
            title: String::new(),
            artist: String::new(),
            ..smtc()
        };
        let v = view(&Link::Ok(s.clone()), ADDR);
        // 只剩专辑时第二行就是专辑，和网页一样。
        assert_eq!((v.line1.as_str(), v.line2.as_str()), ("未知曲目", "叶惠美"));
        assert_eq!(v.caption, "未知曲目");
        let bare = State {
            album: String::new(),
            ..s
        };
        assert_eq!(view(&Link::Ok(bare), ADDR).line2, "");
    }

    #[test]
    fn mediakey_mode_has_no_art_but_works() {
        let s = State {
            mode: Mode::MediaKey,
            playing: true,
            ..State {
                present: true,
                ..Default::default()
            }
        };
        let v = view(&Link::Ok(s), ADDR);
        assert_eq!(v.line1, "QQ音乐 正在播放");
        assert!(v.enabled && !v.art);
        assert_eq!(v.glyph, Glyph::Pause);
    }

    #[test]
    fn absent_player_keeps_buttons_enabled() {
        let v = view(&Link::Ok(State::default()), ADDR);
        assert_eq!(v.line1, "QQ音乐 未在播放");
        assert!(v.enabled && !v.art);
    }

    #[test]
    fn link_failures_disable_buttons() {
        for (link, line1) in [
            (Link::Connecting, "正在连接…"),
            (Link::Unreachable, "连不上服务端"),
            (Link::Unauthorized, "token 无效"),
            (Link::Failed("媒体线程没响应\n\n详情".into()), "服务端出错"),
        ] {
            let v = view(&link, ADDR);
            assert_eq!(v.line1, line1);
            assert!(!v.enabled && !v.art, "{line1}");
        }
        assert_eq!(view(&Link::Unreachable, ADDR).line2, ADDR);
        assert_eq!(view(&Link::Failed("a\nb".into()), ADDR).line2, "a");
    }

    #[test]
    fn progress_extrapolates_only_while_playing() {
        let s = smtc();
        assert_eq!(progress(&s, 0.0), Some(0.05));
        assert_eq!(progress(&s, 10.0), Some(0.1));
        let paused = State {
            playing: false,
            ..smtc()
        };
        assert_eq!(progress(&paused, 10.0), Some(0.05));
        // 外推不越过结尾。
        assert_eq!(progress(&s, 1000.0), Some(1.0));
    }

    #[test]
    fn progress_absent_without_timeline() {
        assert_eq!(
            progress(
                &State {
                    duration: None,
                    ..smtc()
                },
                0.0
            ),
            None
        );
        assert_eq!(
            progress(
                &State {
                    duration: Some(0.0),
                    ..smtc()
                },
                0.0
            ),
            None
        );
        assert_eq!(
            progress(
                &State {
                    mode: Mode::MediaKey,
                    ..smtc()
                },
                0.0
            ),
            None
        );
    }
}
