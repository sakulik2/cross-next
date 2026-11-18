//! 从扁平 JSON 里定向取字段。
//!
//! 本项目收发的 JSON 都是自己产生的、单层、字段已知，所以做的是定向提取而不是
//! 通用解析。配置文件和 `/api/state` 的响应都走这里。

/// 取字符串字段的值，或裸 token（数字、true/false/null）的字面文本。
///
/// 不处理转义 —— 用它读的都是本程序自己写出去的值。
pub fn field(text: &str, key: &str) -> Option<String> {
    let rest = after_key(text, key)?;

    if let Some(s) = rest.strip_prefix('"') {
        return Some(s[..s.find('"')?].to_string());
    }

    // 裸值：读到分隔符为止。
    let end = rest
        .find(|c: char| c == ',' || c == '}' || c.is_whitespace())
        .unwrap_or(rest.len());
    if end == 0 {
        None
    } else {
        Some(rest[..end].to_string())
    }
}

/// 取字符串字段，**处理转义**。值不是字符串（null、数字）时返回 None。
///
/// [`field`] 不处理转义，读自己写的配置够用；但 `/api/state` 里的曲名、歌手来自
/// 媒体文件，服务端会转义其中的 `"` 和 `\`。用 `field` 读 `"Heroes"` 这样的曲名会
/// 在第一个转义引号处截断，所以显示给人看的字符串走这里。
pub fn string(text: &str, key: &str) -> Option<String> {
    let rest = after_key(text, key)?.strip_prefix('"')?;
    let mut out = String::new();
    let mut chars = rest.chars();
    loop {
        match chars.next()? {
            '"' => return Some(out),
            '\\' => match chars.next()? {
                'n' => out.push('\n'),
                'r' => out.push('\r'),
                't' => out.push('\t'),
                'b' => out.push('\u{8}'),
                'f' => out.push('\u{c}'),
                'u' => {
                    let hi = hex4(&mut chars)?;
                    // 基本平面外的字符是一对代理项（如 `🎵`），要拼起来。
                    let c = if (0xD800..0xDC00).contains(&hi) {
                        let mut ahead = chars.clone();
                        let lo = (ahead.next() == Some('\\') && ahead.next() == Some('u'))
                            .then(|| hex4(&mut ahead))
                            .flatten()
                            .filter(|lo| (0xDC00..0xE000).contains(lo));
                        lo.and_then(|lo| {
                            chars = ahead;
                            char::from_u32(0x10000 + ((hi - 0xD800) << 10) + (lo - 0xDC00))
                        })
                    } else {
                        char::from_u32(hi)
                    };
                    out.push(c.unwrap_or('\u{FFFD}'));
                }
                // `\"`、`\\`、`\/`，以及不认识的转义：照字面取。
                other => out.push(other),
            },
            c => out.push(c),
        }
    }
}

fn hex4(chars: &mut std::str::Chars) -> Option<u32> {
    let mut v = 0;
    for _ in 0..4 {
        v = v * 16 + chars.next()?.to_digit(16)?;
    }
    Some(v)
}

/// 取数字字段。`null` 或非数字返回 None —— `/api/state` 的 volume 就可能是 null。
pub fn number(text: &str, key: &str) -> Option<f64> {
    let rest = after_key(text, key)?;
    let end = rest
        .find(|c: char| !matches!(c, '0'..='9' | '.' | '-' | '+' | 'e' | 'E'))
        .unwrap_or(rest.len());
    rest[..end].parse().ok()
}

/// 定位到 `"key":` 之后，跳过空白。
fn after_key<'a>(text: &'a str, key: &str) -> Option<&'a str> {
    let needle = format!("\"{key}\"");
    let rest = &text[text.find(&needle)? + needle.len()..];
    Some(rest.trim_start().strip_prefix(':')?.trim_start())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn string_unescapes() {
        let j = r#"{"title":"\"Heroes\" a\\b\nc","artist":"x"}"#;
        assert_eq!(string(j, "title").as_deref(), Some("\"Heroes\" a\\b\nc"));
        assert_eq!(string(j, "artist").as_deref(), Some("x"));
        // 对照：field 在转义引号处截断，这正是显示用的字符串要换掉它的原因。
        assert_eq!(field(j, "title").as_deref(), Some("\\"));
    }

    #[test]
    fn string_handles_unicode_escapes() {
        assert_eq!(string(r#"{"a":"\u0001x"}"#, "a").as_deref(), Some("\u{1}x"));
        assert_eq!(string(r#"{"a":"🎵"}"#, "a").as_deref(), Some("🎵"));
        // 孤立代理项不 panic，换成替换字符。
        assert_eq!(
            string(r#"{"a":"\ud83cz"}"#, "a").as_deref(),
            Some("\u{FFFD}z")
        );
        assert_eq!(string(r#"{"a":"晴天"}"#, "a").as_deref(), Some("晴天"));
    }

    #[test]
    fn string_rejects_non_strings_and_truncation() {
        assert_eq!(string(r#"{"a":null}"#, "a"), None);
        assert_eq!(string(r#"{"a":1}"#, "a"), None);
        assert_eq!(string(r#"{"a":"unterminated"#, "a"), None);
        assert_eq!(string(r#"{"a":"x\"#, "a"), None);
        assert_eq!(string(r#"{"b":"x"}"#, "a"), None);
    }
}
