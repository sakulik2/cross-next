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
