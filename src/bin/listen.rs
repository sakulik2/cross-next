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
//!
//! 单实例处理：它没有窗口也没有托盘图标，用户看不见也关不掉，所以重复启动时
//! 不该只报个错让人自己去找。启动时会接管上一个实例：
//!
//!   1. 建一个带唯一类名的隐藏窗口当标记。
//!   2. 启动时 `FindWindowW` 找这个类名 —— 找到就是我们自己的旧实例，
//!      发 `WM_CLOSE` 让它自己退出，然后接手热键。
//!
//! 刻意不按进程名匹配再 taskkill：`listen.exe` 这名字太通用，那样会误伤任何同名
//! 的无关程序。唯一类名只会命中我们自己。而且发 WM_CLOSE 让旧实例走正常退出路径，
//! 它能干净地 UnregisterHotKey，强杀做不到这一点。
//!
//! 注意 Win32 **没有**反查热键所有者的 API。`RegisterHotKey` 失败只告诉你失败，
//! 查不出是谁占着。所以只能识别自己的实例，占用方是别的程序时只能如实报告。
//!
//! `listen.exe --stop` 只关掉已有实例然后退出，不启动新的。

#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use cross_next::client;
use std::sync::mpsc::{Sender, channel};
use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, WPARAM};
use windows::Win32::UI::Input::KeyboardAndMouse::{
    HOT_KEY_MODIFIERS, RegisterHotKey, UnregisterHotKey, VK_MEDIA_NEXT_TRACK, VK_MEDIA_PLAY_PAUSE,
    VK_MEDIA_PREV_TRACK,
};
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, DestroyWindow, DispatchMessageW, FindWindowW, GetMessageW,
    MSG, PostMessageW, PostQuitMessage, RegisterClassW, WINDOW_EX_STYLE, WINDOW_STYLE, WM_CLOSE,
    WM_DESTROY, WM_HOTKEY, WNDCLASSW,
};
use windows::core::w;

/// 热键 id 与对应命令。id 只需在本进程内唯一。
const BINDINGS: [(i32, u16, &str, &str); 3] = [
    (1, VK_MEDIA_NEXT_TRACK.0, "next", "下一首"),
    (2, VK_MEDIA_PREV_TRACK.0, "prev", "上一首"),
    (3, VK_MEDIA_PLAY_PAUSE.0, "playpause", "播放/暂停"),
];

/// 隐藏标记窗口的类名。够独特，不会和别的程序撞。
const MARKER_CLASS: windows::core::PCWSTR = w!("CrossNextListenMarker");

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

    let stop_only = std::env::args().skip(1).any(|a| a == "--stop");

    // 先接管旧实例。它退出时会 UnregisterHotKey，所以必须在我们注册之前做完。
    let replaced = stop_previous();

    if stop_only {
        let msg = if replaced {
            "已关闭正在运行的 listen.exe。"
        } else {
            "没有正在运行的 listen.exe。"
        };
        report(msg);
        return;
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

    // 标记窗口要在注册热键之前建好 —— 它同时承担两个职责：让后续实例找到我们，
    // 以及接收 WM_CLOSE 来干净退出。
    let marker = match create_marker() {
        Some(h) => h,
        None => {
            fail("无法创建标记窗口，无法保证单实例。");
            return;
        }
    };

    let mut registered = Vec::new();
    for (id, vk, _, name) in BINDINGS {
        // 无修饰键，直接注册裸媒体键。
        if unsafe { RegisterHotKey(None, id, HOT_KEY_MODIFIERS(0), vk as u32) }.is_ok() {
            registered.push((id, name));
        }
    }

    if registered.is_empty() {
        let _ = unsafe { DestroyWindow(marker) };
        // 走到这里说明占用方不是我们自己的实例（那个已经被关掉了）。
        // Win32 查不出热键属于谁，所以只能列出常见的占用方。
        fail(
            "媒体键全部注册失败 —— 被其它程序独占了。\n\n\
             常见占用方：播放器的全局热键设置、键盘厂商驱动、其它媒体控制小工具。",
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

    announce(&target, &registered, replaced);
    pump(&tx);

    for (id, _) in &registered {
        let _ = unsafe { UnregisterHotKey(None, *id) };
    }
    let _ = unsafe { DestroyWindow(marker) };
}

/// 找到并关掉已有实例，返回是否真的关了一个。
///
/// 发 `WM_CLOSE` 而不是强杀：旧实例走正常退出路径才能 `UnregisterHotKey`，
/// 否则热键可能残留到系统回收为止。
fn stop_previous() -> bool {
    let found = unsafe { FindWindowW(MARKER_CLASS, None) };
    let Ok(existing) = found else {
        return false; // 没有旧实例
    };
    if existing.is_invalid() {
        return false;
    }

    if unsafe { PostMessageW(Some(existing), WM_CLOSE, WPARAM(0), LPARAM(0)) }.is_err() {
        return false;
    }

    // 等它真的放开热键。轮询窗口是否消失，比睡一个固定时长可靠 ——
    // 睡太短会抢不到热键，睡太长则白等。
    for _ in 0..50 {
        std::thread::sleep(std::time::Duration::from_millis(20));
        match unsafe { FindWindowW(MARKER_CLASS, None) } {
            Ok(h) if !h.is_invalid() => continue,
            _ => return true, // 窗口已消失，热键已释放
        }
    }

    // 超时：旧实例卡住了。仍然返回 true，让调用方继续尝试注册 ——
    // 注册失败会给出明确提示，比在这里下结论好。
    true
}

/// 建隐藏标记窗口。它是单实例机制的锚点，不显示任何界面。
fn create_marker() -> Option<HWND> {
    unsafe {
        let wc = WNDCLASSW {
            lpfnWndProc: Some(marker_proc),
            lpszClassName: MARKER_CLASS,
            ..Default::default()
        };
        // 类名已注册返回 0；本进程只注册一次，所以失败即真失败。
        if RegisterClassW(&wc) == 0 {
            return None;
        }

        CreateWindowExW(
            WINDOW_EX_STYLE(0),
            MARKER_CLASS,
            w!("cross-next listen"),
            WINDOW_STYLE(0), // 不带 WS_VISIBLE，永不显示
            0,
            0,
            0,
            0,
            None,
            None,
            None,
            None,
        )
        .ok()
    }
}

/// 标记窗口的消息处理。收到 WM_CLOSE 就退出整个程序。
unsafe extern "system" fn marker_proc(
    hwnd: HWND,
    msg: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    match msg {
        // 新实例请求我们让位。
        WM_CLOSE => {
            let _ = unsafe { DestroyWindow(hwnd) };
            LRESULT(0)
        }
        // 窗口销毁后让消息循环收摊，main 尾部会 UnregisterHotKey。
        WM_DESTROY => {
            unsafe { PostQuitMessage(0) };
            LRESULT(0)
        }
        _ => unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) },
    }
}

/// 消息循环。热键触发时投 WM_HOTKEY 到本线程队列。
fn pump(tx: &Sender<&'static str>) {
    let mut msg = MSG::default();
    // GetMessageW 返回 0 表示收到 WM_QUIT，-1 表示出错。
    while unsafe { GetMessageW(&mut msg, None, 0, 0) }.as_bool() {
        // WM_HOTKEY 是投给线程而非窗口的，所以在这里直接处理。
        if msg.message == WM_HOTKEY {
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
            continue;
        }

        // 其余消息派发给窗口过程 —— 标记窗口的 WM_CLOSE 靠这一步才能收到，
        // 少了它单实例接管就完全不生效。
        unsafe { DispatchMessageW(&msg) };
    }
}

fn announce(target: &str, registered: &[(i32, &str)], replaced: bool) {
    // 窗口子系统下这些 println 只在从命令行启动时可见，正好用于首次验证。
    println!("cross-next 媒体键转发已启动");
    if replaced {
        println!("  （已接管上一个实例）");
    }
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
    println!("  关掉本程序即恢复 —— 没有界面，用 listen.exe --stop。");
}

/// 提示性消息（非错误）。与 fail 共用输出通道的选择逻辑。
fn report(msg: &str) {
    if attach_console() {
        println!("{msg}");
        return;
    }

    use windows::Win32::UI::WindowsAndMessaging::{MB_ICONINFORMATION, MB_OK, MessageBoxW};
    use windows::core::HSTRING;

    let text = HSTRING::from(msg);
    let title = HSTRING::from("cross-next listen");
    unsafe {
        MessageBoxW(None, &text, &title, MB_OK | MB_ICONINFORMATION);
    }
}

/// 尝试附加到父进程控制台。成功表示这是从命令行启动的，那时打印比弹框有用。
fn attach_console() -> bool {
    use windows::Win32::System::Console::{ATTACH_PARENT_PROCESS, AttachConsole};

    cfg!(debug_assertions) || unsafe { AttachConsole(ATTACH_PARENT_PROCESS) }.is_ok()
}

/// 启动期的致命错误。双击运行时没有控制台，所以用消息框。
fn fail(msg: &str) {
    if attach_console() {
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
