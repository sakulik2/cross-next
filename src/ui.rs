//! 窗口子系统程序的输出通道选择：有控制台就打印，没有就弹消息框。
//!
//! 三个 exe（`cross-next`、`remote`、`listen`）都编成窗口子系统，都面临同一个问题：
//! 双击运行时没有控制台，`eprintln!` 掉进虚空，用户看到的是「点了没反应」。
//! 这里集中处理，免得三份各自漂移。

use std::sync::OnceLock;

/// 版本号，编译期从 `Cargo.toml` 取。
///
/// 五个 binary 共享它，所以「发布了但忘了 bump」不会在这里分叉 —— 那个错误由
/// release 工作流的守卫拦（见 CLAUDE.md）。暴露版本号的实际用处是更新之后能确认
/// 跑的到底是哪一份:exe 被覆盖了但旧进程还在跑,光看文件时间戳判断不出来。
pub const VERSION: &str = env!("CARGO_PKG_VERSION");

/// 本进程是否有可用的控制台。
///
/// 窗口子系统程序默认没有控制台，`AttachConsole(ATTACH_PARENT_PROCESS)` 能借用
/// 调用方的。双击启动时父进程是资源管理器，没有控制台，于是失败 —— 正好当判据。
///
/// **结果必须缓存**：`AttachConsole` 成功后再调一次会失败（已经有控制台了），
/// 所以直接每次调用系统 API 的写法会让第二处及以后的判断全部误判成"没有控制台"，
/// 于是命令行用户开始莫名收到消息框。
pub fn has_console() -> bool {
    static CACHED: OnceLock<bool> = OnceLock::new();

    *CACHED.get_or_init(|| {
        use windows::Win32::System::Console::{ATTACH_PARENT_PROCESS, AttachConsole};

        // 调试构建本来就是控制台子系统，不必绕这一圈。
        if cfg!(debug_assertions) {
            return true;
        }

        unsafe { AttachConsole(ATTACH_PARENT_PROCESS) }.is_ok()
    })
}

/// 错误。有控制台走 stderr（可复制、可管道），否则弹警告框。
pub fn fail(title: &str, msg: &str) {
    if has_console() {
        eprintln!("{msg}");
        return;
    }
    box_message(title, msg, true);
}

/// 提示性消息（非错误）。通道选择同 [`fail`]。
pub fn report(title: &str, msg: &str) {
    if has_console() {
        println!("{msg}");
        return;
    }
    box_message(title, msg, false);
}

/// 无条件弹消息框，不看有没有控制台。
///
/// 给托盘菜单用：那时用户是在图形界面上点的，回答就该出现在图形界面上，
/// 哪怕这个进程恰好还连着一个控制台。
pub fn box_message(title: &str, msg: &str, warn: bool) {
    use windows::Win32::UI::WindowsAndMessaging::{
        MB_ICONINFORMATION, MB_ICONWARNING, MB_OK, MessageBoxW,
    };
    use windows::core::HSTRING;

    let text = HSTRING::from(msg);
    let title = HSTRING::from(title);
    let icon = if warn {
        MB_ICONWARNING
    } else {
        MB_ICONINFORMATION
    };
    unsafe {
        MessageBoxW(None, &text, &title, MB_OK | icon);
    }
}

/// `listen.exe` 隐藏标记窗口的类名。`listen` 靠它接管自己的旧实例，`desktop` 靠它
/// 找到正在运行的 `listen` 并关掉 —— 两者都要独占媒体键，同时开着只有一个拿得到。
pub const LISTEN_MARKER: windows::core::PCWSTR = windows::core::w!("CrossNextListenMarker");

/// 找到某个窗口类的窗口，发 `WM_CLOSE` 请它自己退出，轮询到窗口消失（最多
/// `wait`）。返回是否找到并通知了一个。
///
/// 发 `WM_CLOSE` 而不是强杀：对方走正常退出路径，才会 `UnregisterHotKey`。超时仍返回
/// true —— 确实通知到了，调用方接下来的注册失败会比这里的布尔值更能说明问题。
pub fn close_instance(class: windows::core::PCWSTR, wait: std::time::Duration) -> bool {
    use windows::Win32::Foundation::{LPARAM, WPARAM};
    use windows::Win32::UI::WindowsAndMessaging::{FindWindowW, PostMessageW, WM_CLOSE};

    let find = || {
        unsafe { FindWindowW(class, None) }
            .ok()
            .filter(|h| !h.is_invalid())
    };
    let Some(h) = find() else { return false };
    if unsafe { PostMessageW(Some(h), WM_CLOSE, WPARAM(0), LPARAM(0)) }.is_err() {
        return false;
    }
    let step = std::time::Duration::from_millis(20);
    let mut waited = std::time::Duration::ZERO;
    while waited < wait && find().is_some() {
        std::thread::sleep(step);
        waited += step;
    }
    true
}

/// 声明 Per-Monitor-v2 DPI 感知。
///
/// 不声明的话高分屏上消息框与菜单会被系统按 96 DPI 渲染再拉伸，字发虚。
/// **必须在创建任何窗口之前调用**，所以每个 exe 的 main 开头第一件事就是它。
pub fn init_dpi() {
    use windows::Win32::UI::HiDpi::{
        DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2, SetProcessDpiAwarenessContext,
    };

    // 失败不影响功能（只是显示模糊），所以忽略返回值。
    unsafe {
        let _ = SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2);
    }
}
