//! `keyprobe.exe` —— 探测媒体键到底走哪条通路。
//!
//! 背景：有些鼠标/键盘驱动把「上一首/下一首」实现为标准键盘事件（`VK_MEDIA_*`），
//! 有些直接发 `WM_APPCOMMAND`。两者需要完全不同的截获方式，而驱动通常不说自己
//! 走哪条。这个程序同时挂上三条通路，按一下键就知道了。
//!
//!   1. 低级键盘钩子（WH_KEYBOARD_LL）—— 能看到键盘事件
//!   2. Shell 钩子（RegisterShellHookWindow）—— 能看到 HSHELL_APPCOMMAND
//!   3. RegisterHotKey —— 系统级热键注册，能独占媒体键
//!
//! 用法：运行它，然后按鼠标侧键（或键盘媒体键），看哪一行有输出。Ctrl+C 退出。

use std::sync::OnceLock;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::mpsc::{Sender, channel};
use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, WPARAM};
use windows::Win32::UI::Input::KeyboardAndMouse::{
    HOT_KEY_MODIFIERS, RegisterHotKey, VK_MEDIA_NEXT_TRACK, VK_MEDIA_PLAY_PAUSE,
    VK_MEDIA_PREV_TRACK,
};
use windows::Win32::UI::WindowsAndMessaging::{
    CallNextHookEx, CreateWindowExW, DefWindowProcW, GetMessageW, KBDLLHOOKSTRUCT, MSG,
    RegisterClassW, RegisterShellHookWindow, RegisterWindowMessageW, SetWindowsHookExW,
    WH_KEYBOARD_LL, WINDOW_EX_STYLE, WINDOW_STYLE, WM_APPCOMMAND, WM_HOTKEY, WM_KEYDOWN,
    WM_SYSKEYDOWN, WNDCLASSW,
};
use windows::core::w;

/// 钩子回调拿不到自定义参数，只能走全局。钩子和窗口过程可能在不同线程被调用，
/// 所以用线程安全的容器而不是 `static mut`（后者多线程访问是 UB）。
static REPORTER: OnceLock<Sender<String>> = OnceLock::new();
/// Shell 钩子的消息号由 RegisterWindowMessageW 动态分配，窗口过程里要用。
static SHELL_MSG: AtomicU32 = AtomicU32::new(0);

/// HSHELL_APPCOMMAND，来自 shell 钩子通知。
const HSHELL_APPCOMMAND: u32 = 12;

fn main() {
    println!("cross-next 媒体键探测器\n");
    println!("现在按鼠标侧键（或键盘媒体键），看下面哪条通路有反应。");
    println!("Ctrl+C 退出。\n");

    let (tx, rx) = channel::<String>();

    // 报告线程：钩子回调里不该做 IO，把消息传出来再打印。
    std::thread::spawn(move || {
        for line in rx {
            println!("{line}");
        }
    });

    // 只设一次，失败说明已经设过了，不可能发生。
    let _ = REPORTER.set(tx);

    // ---- 通路 1：低级键盘钩子 ----
    let hook = unsafe { SetWindowsHookExW(WH_KEYBOARD_LL, Some(keyboard_hook), None, 0) };
    match &hook {
        Ok(_) => println!("[1] 低级键盘钩子: 已挂载"),
        Err(e) => println!("[1] 低级键盘钩子: 挂载失败 {e}"),
    }

    // ---- 通路 3：系统热键 ----
    // 注册在消息循环所属线程上，WM_HOTKEY 会投到该线程的队列。
    // 注意这会「独占」这些键 —— 注册成功后 QQ音乐 自己就收不到了，
    // 这正是将来做转发时想要的效果，但探测阶段要意识到这个副作用。
    let keys = [
        (1, VK_MEDIA_NEXT_TRACK.0, "下一首"),
        (2, VK_MEDIA_PREV_TRACK.0, "上一首"),
        (3, VK_MEDIA_PLAY_PAUSE.0, "播放/暂停"),
    ];
    let mut hotkey_ok = 0;
    for (id, vk, name) in keys {
        // 无修饰键，直接注册裸媒体键。
        if unsafe { RegisterHotKey(None, id, HOT_KEY_MODIFIERS(0), vk as u32) }.is_ok() {
            hotkey_ok += 1;
        } else {
            println!("[3] 热键 {name}: 注册失败（可能已被其它程序占用）");
        }
    }
    println!("[3] 系统热键: {hotkey_ok}/3 注册成功");

    // ---- 通路 2：Shell 钩子（需要一个窗口）----
    let hwnd = create_message_window();
    match hwnd {
        Some(h) => {
            let shell_msg = unsafe { RegisterWindowMessageW(w!("SHELLHOOK")) };
            SHELL_MSG.store(shell_msg, Ordering::Relaxed);
            let ok = unsafe { RegisterShellHookWindow(h) }.as_bool();
            println!("[2] Shell 钩子: {}", if ok { "已注册" } else { "注册失败" });
        }
        None => println!("[2] Shell 钩子: 建窗口失败，跳过"),
    }

    println!("\n等待按键……\n");

    // 消息循环：热键和 shell 钩子都靠它派发。
    let mut msg = MSG::default();
    unsafe {
        while GetMessageW(&mut msg, None, 0, 0).as_bool() {
            if msg.message == WM_HOTKEY {
                let name = match msg.wParam.0 {
                    1 => "下一首",
                    2 => "上一首",
                    3 => "播放/暂停",
                    _ => "未知",
                };
                report(format!("  [3] 系统热键 触发: {name}"));
            }
            windows::Win32::UI::WindowsAndMessaging::DispatchMessageW(&msg);
        }
    }

    if let Ok(h) = hook {
        let _ = unsafe { windows::Win32::UI::WindowsAndMessaging::UnhookWindowsHookEx(h) };
    }
}

fn report(line: String) {
    // 钩子回调可能在别的线程跑，用通道把打印挪出去。
    if let Some(tx) = REPORTER.get() {
        let _ = tx.send(line);
    }
}

/// 低级键盘钩子回调。只报媒体键，别的按键会淹没输出。
unsafe extern "system" fn keyboard_hook(code: i32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    // code < 0 表示必须直接转交，不能检查内容。
    if code >= 0 && (wparam.0 as u32 == WM_KEYDOWN || wparam.0 as u32 == WM_SYSKEYDOWN) {
        let kb = unsafe { &*(lparam.0 as *const KBDLLHOOKSTRUCT) };
        let name = match kb.vkCode as u16 {
            176 => Some("下一首 (VK_MEDIA_NEXT_TRACK)"),
            177 => Some("上一首 (VK_MEDIA_PREV_TRACK)"),
            179 => Some("播放/暂停 (VK_MEDIA_PLAY_PAUSE)"),
            _ => None,
        };
        if let Some(name) = name {
            report(format!("  [1] 键盘钩子 捕获: {name}"));
        }
    }

    // 必须继续传递，否则会吞掉全系统的按键。
    unsafe { CallNextHookEx(None, code, wparam, lparam) }
}

/// 建一个不可见的消息窗口，仅用于接收 shell 钩子通知。
fn create_message_window() -> Option<HWND> {
    unsafe {
        let class_name = w!("CrossNextKeyProbe");
        let wc = WNDCLASSW {
            lpfnWndProc: Some(window_proc),
            lpszClassName: class_name,
            ..Default::default()
        };
        if RegisterClassW(&wc) == 0 {
            return None;
        }

        CreateWindowExW(
            WINDOW_EX_STYLE(0),
            class_name,
            w!("keyprobe"),
            WINDOW_STYLE(0),
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

unsafe extern "system" fn window_proc(
    hwnd: HWND,
    msg: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    let shell_msg = SHELL_MSG.load(Ordering::Relaxed);

    // Shell 钩子把 APPCOMMAND 包在动态消息号里转发。
    if shell_msg != 0 && msg == shell_msg && wparam.0 as u32 == HSHELL_APPCOMMAND {
        // 命令码在 lParam 的高位字，还要滤掉 FAPPCOMMAND 掩码。
        let cmd = ((lparam.0 >> 16) & 0x0fff) as u32;
        report(format!("  [2] Shell 钩子 捕获: {}", appcommand_name(cmd)));
        return LRESULT(1);
    }

    // 直接投给本窗口的 WM_APPCOMMAND（正常不会发生，记一笔以防万一）。
    if msg == WM_APPCOMMAND {
        let cmd = ((lparam.0 >> 16) & 0x0fff) as u32;
        report(format!(
            "  [2] 直接 WM_APPCOMMAND: {}",
            appcommand_name(cmd)
        ));
        return LRESULT(1);
    }

    unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) }
}

/// APPCOMMAND_* 常量名。只列媒体相关的。
fn appcommand_name(cmd: u32) -> String {
    match cmd {
        11 => "下一首 (APPCOMMAND_MEDIA_NEXTTRACK)".into(),
        12 => "上一首 (APPCOMMAND_MEDIA_PREVIOUSTRACK)".into(),
        13 => "停止 (APPCOMMAND_MEDIA_STOP)".into(),
        14 => "播放/暂停 (APPCOMMAND_MEDIA_PLAY_PAUSE)".into(),
        46 => "播放 (APPCOMMAND_MEDIA_PLAY)".into(),
        47 => "暂停 (APPCOMMAND_MEDIA_PAUSE)".into(),
        other => format!("APPCOMMAND 码 {other}"),
    }
}
