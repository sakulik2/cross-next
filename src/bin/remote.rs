//! `remote.exe` —— 一次性发一条命令给 cross-next，然后退出。
//!
//! 这是给鼠标/键盘驱动用的：把侧键绑到「启动程序」，参数填 next 或 prev。
//! 按一下就切一首歌，不需要常驻进程，也不需要键盘钩子 —— 驱动到底发什么键码
//! 这个不确定性直接绕过了。
//!
//!   remote.exe next
//!   remote.exe prev
//!   remote.exe playpause
//!   remote.exe vol +10      音量相对调整（百分点）
//!   remote.exe vol 60       音量设为 60%
//!   remote.exe mute
//!
//! 服务器地址与 token 从同目录的 remote.json 读，没有就按提示生成一份。
//!
//! 编译成 Windows 子系统（见 main 上方的 windows_subsystem 属性），所以按下去
//! 不会闪一个黑框。出错时用消息框提示，因为这种场景下没有控制台可看。

#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use cross_next::client;

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

    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.is_empty() {
        fail(&usage());
        return;
    }

    let config = match client::load_config() {
        Ok(c) => c,
        Err(e) => {
            fail(&e);
            return;
        }
    };

    if let Err(e) = client::dispatch(&config, &args) {
        fail(&format!(
            "{e}

{}",
            usage()
        ));
    }
}

fn usage() -> String {
    "用法：

       remote.exe next
       remote.exe prev
       remote.exe playpause
       remote.exe vol +10     音量加 10 个百分点
       remote.exe vol 60      音量设为 60%
       remote.exe mute        静音开关

     驱动没有「启动程序」选项时，改用 listen.exe（常驻，抢媒体键转发）。
"
    .to_string()
}

/// 报错。
///
/// 这个程序编译成窗口子系统，双击运行时没有控制台，所以默认弹消息框。但从命令行
/// 跑（配置阶段一定会这么跑）时它能附加到父进程的控制台 —— 那种情况下打印比弹框
/// 有用得多，输出可以复制、可以管道。
fn fail(msg: &str) {
    if attach_console() {
        eprintln!("{msg}");
        return;
    }

    use windows::Win32::UI::WindowsAndMessaging::{MB_ICONWARNING, MB_OK, MessageBoxW};
    use windows::core::HSTRING;

    let text = HSTRING::from(msg);
    let title = HSTRING::from("cross-next remote");
    unsafe {
        MessageBoxW(None, &text, &title, MB_OK | MB_ICONWARNING);
    }
}

/// 尝试附加到父进程的控制台。成功表示这是从命令行启动的。
///
/// 窗口子系统程序默认没有控制台，`AttachConsole(ATTACH_PARENT_PROCESS)` 能借用
/// 调用方的。双击启动时父进程是资源管理器，没有控制台，于是失败 —— 正好当判据。
fn attach_console() -> bool {
    use windows::Win32::System::Console::{ATTACH_PARENT_PROCESS, AttachConsole};

    // 调试构建直接走 stderr，不必绕这一圈。
    if cfg!(debug_assertions) {
        return true;
    }

    unsafe { AttachConsole(ATTACH_PARENT_PROCESS).is_ok() }
}
