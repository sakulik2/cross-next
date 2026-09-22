//! `listen.exe` —— 常驻本机，抢下媒体键转发给台式机上的 cross-next。
//!
//! 为什么需要它：有些鼠标驱动只能把侧键绑到预设的「上一首/下一首」
//! 功能，没有「启动程序」这一项，所以没法直接调 remote.exe。驱动发出的是标准
//! 媒体键，那就在本机把这些键抢下来再转发。
//!
//! 用 `RegisterHotKey` 而不是键盘钩子：
//!   - 它**独占**按键，本机播放器收不到。这正是想要的 —— 笔记本音响烂，
//!     按键应该只作用于台式机，不该把本地播放器也带起来。
//!   - 键盘钩子只能旁听，要拦截得返回非零值，而那会影响全系统按键处理。
//!   - 实测（keyprobe）三个媒体键都能注册成功。
//!
//! 转发在独立线程里做，不阻塞消息循环 —— 网络请求约 50ms，
//! 连按时不该丢键。

#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use cross_next::client;
use std::sync::mpsc::{Sender, channel};
use windows::Win32::UI::Input::KeyboardAndMouse::{
    HOT_KEY_MODIFIERS, RegisterHotKey, UnregisterHotKey, VK_MEDIA_NEXT_TRACK, VK_MEDIA_PLAY_PAUSE,
    VK_MEDIA_PREV_TRACK,
};
use windows::Win32::UI::WindowsAndMessaging::{GetMessageW, MSG, WM_HOTKEY};

/// 热键 id 与对应命令。id 只需在本进程内唯一。
const BINDINGS: [(i32, u16, &str, &str); 3] = [
    (1, VK_MEDIA_NEXT_TRACK.0, "next", "下一首"),
    (2, VK_MEDIA_PREV_TRACK.0, "prev", "上一首"),
    (3, VK_MEDIA_PLAY_PAUSE.0, "playpause", "播放/暂停"),
];

fn main() {
    // 声明 DPI 感知，否则高分屏上消息框会被系统按 96 DPI 渲染再放大，字发虚。
    // 必须在创建任何窗口之前调用。
    unsafe {
        use windows::Win32::UI::HiDpi::{
            DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2, SetProcessDpiAwarenessContext,
        };
        // 失败不影响功能（只是显示模糊），所以忽略返回值。
        let _ = SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2);
    }

    // 配置在启动时就校验，不要等到第一次按键才报错。
    let config = match client::load_config() {
        Ok(c) => c,
        Err(e) => {
            fail(&e);
            return;
        }
    };

    let target = format!("{}:{}", config.host, config.port);

    let mut registered = Vec::new();
    for (id, vk, _, name) in BINDINGS {
        // 无修饰键，直接注册裸媒体键。
        if unsafe { RegisterHotKey(None, id, HOT_KEY_MODIFIERS(0), vk as u32) }.is_ok() {
            registered.push((id, name));
        }
    }

    if registered.is_empty() {
        fail(
            "媒体键全部注册失败 —— 可能已被其它程序独占\
             （另一个 listen.exe？播放器的全局热键？）。\n\
             关掉那个程序再试。",
        );
        return;
    }

    // 转发线程：网络往返约 50ms，放在消息循环里会拖慢连按。
    let (tx, rx) = channel::<&'static str>();
    std::thread::spawn(move || {
        for action in rx {
            if let Err(e) = client::dispatch(&config, &[action.to_string()]) {
                // 常驻程序不该为单次失败弹框打扰人，打到控制台（若有）即可。
                eprintln!("[{action}] 失败: {e}");
            }
        }
    });

    announce(&target, &registered);
    pump(&tx);

    for (id, _) in &registered {
        let _ = unsafe { UnregisterHotKey(None, *id) };
    }
}

/// 消息循环。热键触发时投 WM_HOTKEY 到本线程队列。
fn pump(tx: &Sender<&'static str>) {
    let mut msg = MSG::default();
    // GetMessageW 返回 0 表示收到 WM_QUIT，-1 表示出错。
    while unsafe { GetMessageW(&mut msg, None, 0, 0) }.as_bool() {
        if msg.message != WM_HOTKEY {
            continue;
        }
        // wParam 是注册时给的 id。
        if let Some((_, _, action, name)) =
            BINDINGS.iter().find(|(id, ..)| *id == msg.wParam.0 as i32)
        {
            println!("{name} -> 转发");
            // 送不出去说明转发线程没了，那时退出循环。
            if tx.send(action).is_err() {
                break;
            }
        }
    }
}

fn announce(target: &str, registered: &[(i32, &str)]) {
    // 窗口子系统下这些 println 只在从命令行启动时可见，正好用于首次验证。
    println!("cross-next 媒体键转发已启动");
    println!();
    println!("  转发目标: {target}");
    print!("  已独占:  ");
    for (i, (_, name)) in registered.iter().enumerate() {
        if i > 0 {
            print!("、");
        }
        print!("{name}");
    }
    println!();
    if registered.len() < BINDINGS.len() {
        println!("  注意: 部分媒体键注册失败，已被其它程序占用。");
    }
    println!();
    println!("  这些键现在只控制台式机，本机播放器收不到。");
    println!("  关掉本程序即恢复。");
}

/// 启动期的致命错误。双击运行时没有控制台，所以用消息框。
fn fail(msg: &str) {
    use windows::Win32::System::Console::{ATTACH_PARENT_PROCESS, AttachConsole};

    // 从命令行启动时能附加到父进程控制台，那种情况下打印比弹框有用。
    let has_console =
        cfg!(debug_assertions) || unsafe { AttachConsole(ATTACH_PARENT_PROCESS) }.is_ok();
    if has_console {
        eprintln!("{msg}");
        return;
    }

    use windows::Win32::UI::WindowsAndMessaging::{MB_ICONWARNING, MB_OK, MessageBoxW};
    use windows::core::HSTRING;

    let text = HSTRING::from(msg);
    let title = HSTRING::from("cross-next listen");
    unsafe {
        MessageBoxW(None, &text, &title, MB_OK | MB_ICONWARNING);
    }
}
