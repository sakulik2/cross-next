//! 托盘图标：让服务端能长期开着而不占一个控制台窗口。
//!
//! 服务端是要在放音乐那台机器上常开的东西，一个黑框占着任务栏很碍事。改成窗口
//! 子系统之后所有 `println!` 在双击运行时都无声，而启动横幅本来是用户拿到访问 URL
//! 的唯一途径 —— 所以托盘不只是"好看"，它是替代那条途径的：右键菜单里能取回地址。
//!
//! 启动时刻意**不**接管已有实例（`listen.exe` 那套）：端口绑定天然独占，第二个实例
//! 在 `http::bind` 就会拿到 `AddrInUse`，那里已经有对应提示。`listen` 需要接管是因为
//! 它没有界面、用户看不见也关不掉；服务端有托盘图标，重复启动时静默顶掉一个正在
//! 服务的实例是帮倒忙，而且会破坏「改 port 再开一个」这种明确支持的用法。
//!
//! 但托盘窗口的类名兼作单实例锚点，供 `--stop` 用 —— 见 [`stop_running`]。换 exe 前
//! 得先让旧进程松开文件锁，那是唯一真正需要「关掉它」的场景。
//!
//! 几个踩过的点：
//!
//! - 窗口不能用 `HWND_MESSAGE`。仅消息窗口进不了前台，而 `TrackPopupMenu` 要求
//!   调用方是前台窗口，否则菜单点别处不消失。所以用普通窗口，只是不带 `WS_VISIBLE`。
//! - 退出路径必须 `NIM_DELETE`。少了它托盘区会留一个点不掉的幽灵图标，
//!   要等鼠标划过去系统才发现进程没了。
//! - `TaskbarCreated` 必须处理，见 [`run`] 里的注释。

use crate::ui;
use std::net::SocketAddr;
use std::sync::OnceLock;
use std::sync::atomic::{AtomicIsize, AtomicU32, Ordering};
use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, POINT, WPARAM};
use windows::Win32::UI::Shell::{
    NIF_ICON, NIF_MESSAGE, NIF_TIP, NIM_ADD, NIM_DELETE, NOTIFYICONDATAW, Shell_NotifyIconW,
};
use windows::Win32::UI::WindowsAndMessaging::{
    AppendMenuW, CreatePopupMenu, CreateWindowExW, DefWindowProcW, DestroyMenu, DestroyWindow,
    DispatchMessageW, GetCursorPos, GetMessageW, GetSystemMetrics, HICON, MF_SEPARATOR, MF_STRING,
    MSG, PostMessageW, PostQuitMessage, RegisterClassW, RegisterWindowMessageW, SM_CXSMICON,
    SM_CYSMICON, SW_SHOWNORMAL, SetForegroundWindow, TPM_RIGHTBUTTON, TrackPopupMenu,
    TranslateMessage, WINDOW_EX_STYLE, WINDOW_STYLE, WM_COMMAND, WM_DESTROY, WM_LBUTTONDBLCLK,
    WM_NULL, WM_RBUTTONUP, WNDCLASSW,
};
use windows::core::{PCWSTR, w};

const CLASS_NAME: PCWSTR = w!("CrossNextTrayWindow");
const TITLE: &str = "cross-next";

/// 托盘图标的回调消息。图标事件都从这个消息进来。
const WM_TRAY: u32 = windows::Win32::UI::WindowsAndMessaging::WM_APP + 1;

/// 托盘图标 id。本进程只有一个图标，取值随意。
const ICON_ID: u32 = 1;

// 菜单项 id。
const CMD_ADDRESS: usize = 1;
const CMD_OPEN: usize = 2;
const CMD_QUIT: usize = 3;

/// 编进 exe 的托盘图标。由 `tools/make-icon.mjs` 生成 —— 改图标要重跑那个脚本
/// 并提交产物，否则 exe 里还是旧的。
const ICON_BYTES: &[u8] = include_bytes!("../assets/tray.ico");

/// 菜单要用到的文本。窗口过程是裸函数，拿不到 `run` 的局部变量，所以放静态里。
struct Ctx {
    /// 带 token 的完整访问地址。
    url: String,
    /// 不带 token 的地址，用于 tooltip —— token 不该在鼠标一划就示人。
    bare: String,
}

static CTX: OnceLock<Ctx> = OnceLock::new();

/// 图标句柄。`TaskbarCreated` 后重新 `NIM_ADD` 要用，所以得存下来。
/// `HICON` 不是 `Sync`，存裸指针值本身。
static ICON: AtomicIsize = AtomicIsize::new(0);

/// `TaskbarCreated` 的消息号，`RegisterWindowMessageW` 在运行时才给得出。
static TASKBAR_CREATED: AtomicU32 = AtomicU32::new(0);

/// 关掉正在运行的实例，返回是否真的关了一个。
///
/// 给 `--stop` 用。存在的理由只有一个：解压覆盖 `cross-next.exe` 之前得让旧进程松开
/// 文件锁 —— 运行中的 exe 是被锁住的，直接覆盖会失败。手点托盘的「退出」效果一样，
/// 但那没法写进脚本。
///
/// 发 `WM_CLOSE` 而不是强杀：旧实例走正常退出路径才会 `NIM_DELETE` 摘掉托盘图标，
/// 强杀会留一个幽灵图标，得等鼠标划过去系统才发现进程没了。同理也刻意不按进程名
/// 匹配 —— 类名只会命中我们自己。
///
/// 注意这里找的是**任意**一个实例，不区分端口。多开（改 port）的情况下每次只关一个，
/// 反复调用即可逐个关掉。这是 `FindWindowW` 的能力边界，而多开本身是罕见用法。
pub fn stop_running() -> bool {
    use windows::Win32::UI::WindowsAndMessaging::{FindWindowW, WM_CLOSE};

    let found = unsafe { FindWindowW(CLASS_NAME, None) };
    let Ok(existing) = found else {
        return false; // 没有在跑的实例
    };
    if existing.is_invalid() {
        return false;
    }

    if unsafe { PostMessageW(Some(existing), WM_CLOSE, WPARAM(0), LPARAM(0)) }.is_err() {
        return false;
    }

    // 等它真的退出。轮询窗口是否消失，比睡一个固定时长可靠 —— 调用方紧接着就要
    // 覆盖 exe 文件，返回太早的话文件锁还没放开。
    //
    // 窗口消失只说明消息循环收摊了，进程退出还差一瞬；不过 exe 的锁是内核随进程
    // 对象释放的，这点延迟由调用方的文件操作自然吸收（更新脚本总要解压）。
    for _ in 0..100 {
        std::thread::sleep(std::time::Duration::from_millis(20));
        match unsafe { FindWindowW(CLASS_NAME, None) } {
            Ok(h) if !h.is_invalid() => continue,
            _ => return true,
        }
    }

    // 超时：旧实例卡住了（HTTP 线程阻塞在某个 WinRT 调用上也可能拖住退出）。
    // 仍然返回 true —— 我们确实找到并通知了一个实例，让调用方据此继续，
    // 覆盖失败时的文件错误比这里含糊的布尔值更能说明问题。
    true
}

/// 起托盘并跑消息循环。用户选「退出」时返回。
///
/// 必须在**主线程**调用：消息循环得有个线程专职干这件事，而 HTTP 服务端已经搬到
/// 后台线程去了。返回即可让 `main` 结束进程。
pub fn run(addr: SocketAddr, token: &str) {
    let _ = CTX.set(Ctx {
        url: format!("http://{addr}/?t={token}"),
        bare: format!("http://{addr}/"),
    });

    // 资源管理器崩溃重启后托盘区会被重建，之前 NIM_ADD 的图标随之消失。系统靠广播
    // 这个注册消息通知各程序重新添加 —— 不处理的话图标就永久没了，而进程还在跑，
    // 用户既看不见也关不掉。
    TASKBAR_CREATED.store(
        unsafe { RegisterWindowMessageW(w!("TaskbarCreated")) },
        Ordering::Relaxed,
    );

    let Some(hwnd) = create_window() else {
        // 托盘建不起来不该让服务端跟着死 —— 遥控功能本身不依赖它。
        ui::fail(
            TITLE,
            "托盘图标创建失败，服务端仍在运行。\n\n关闭请用任务管理器结束 cross-next.exe。",
        );
        // 没有窗口就没有消息循环可跑，但 HTTP 线程还在。挂住主线程，
        // 不然进程一退整个服务就没了。
        loop {
            std::thread::park();
        }
    };

    // 自制图标解不出来时退回系统默认图标 —— 宁可难看，也不能没有：
    // 图标是菜单的唯一入口，没有图标等于进程没法关。
    let icon = load_icon().or_else(system_icon).unwrap_or_default();
    ICON.store(icon.0 as isize, Ordering::Relaxed);
    add_icon(hwnd);

    let mut msg = MSG::default();
    // 返回 0 是 WM_QUIT，-1 是出错，两者都该收摊。
    while unsafe { GetMessageW(&mut msg, None, 0, 0) }.as_bool() {
        let _ = unsafe { TranslateMessage(&msg) };
        unsafe { DispatchMessageW(&msg) };
    }
}

fn create_window() -> Option<HWND> {
    unsafe {
        let wc = WNDCLASSW {
            lpfnWndProc: Some(wndproc),
            lpszClassName: CLASS_NAME,
            ..Default::default()
        };
        // 类名已注册时返回 0；本进程只注册一次，所以失败即真失败。
        if RegisterClassW(&wc) == 0 {
            return None;
        }

        CreateWindowExW(
            WINDOW_EX_STYLE(0),
            CLASS_NAME,
            w!("cross-next"),
            WINDOW_STYLE(0), // 不带 WS_VISIBLE，永不显示
            0,
            0,
            0,
            0,
            None, // 刻意不是 HWND_MESSAGE，见模块头注释
            None,
            None,
            None,
        )
        .ok()
    }
}

/// 从内嵌的 .ico 里挑一张合适尺寸的，造出 `HICON`。
///
/// `CreateIconFromResourceEx` 要的是**单张图**的资源位，不是带目录头的 .ico 文件 ——
/// 整个文件喂进去会失败。所以这里手解 `ICONDIR`：6 字节头，之后每张图一条 16 字节的
/// `ICONDIRENTRY`。走这条路是为了不引入 winres 之类的构建期依赖。
fn load_icon() -> Option<HICON> {
    use windows::Win32::UI::WindowsAndMessaging::{CreateIconFromResourceEx, IMAGE_FLAGS};

    let want = unsafe { GetSystemMetrics(SM_CXSMICON) }.max(16);
    let want_y = unsafe { GetSystemMetrics(SM_CYSMICON) }.max(16);

    let bits = pick_image(ICON_BYTES, want)?;

    // dwVer 固定 0x00030000 —— 这是 .ico 资源格式的版本号，不是系统版本。
    unsafe { CreateIconFromResourceEx(bits, true, 0x0003_0000, want, want_y, IMAGE_FLAGS(0)) }.ok()
}

/// 从 .ico 的字节里切出宽度最接近 `want` 的那张图。
///
/// 单独拎出来是为了可测：字节偏移算错不会报错，只会让图标神秘地不出现，
/// 而那要跑起来才看得见。下面的测试直接拿真实的 `assets/tray.ico` 验。
///
/// 所有取值都走 `get`，畸形文件返回 `None` 而不是 panic —— 这个字节数组是编进
/// exe 的，不该崩，但真崩了就把托盘搭进去了。
fn pick_image(ico: &[u8], want: i32) -> Option<&[u8]> {
    let count = u16::from_le_bytes(ico.get(4..6)?.try_into().ok()?) as usize;

    // 挑宽度最接近的一张。宽度字段只有一个字节，0 表示 256。
    let mut best: Option<(i32, usize, usize)> = None; // (差值, offset, len)
    for i in 0..count {
        let e = ico.get(6 + i * 16..6 + i * 16 + 16)?;
        let width = if e[0] == 0 { 256 } else { e[0] as i32 };
        let len = u32::from_le_bytes(e[8..12].try_into().ok()?) as usize;
        let off = u32::from_le_bytes(e[12..16].try_into().ok()?) as usize;
        let diff = (width - want).abs();
        if best.is_none_or(|(d, ..)| diff < d) {
            best = Some((diff, off, len));
        }
    }

    let (_, off, len) = best?;
    ico.get(off..off.checked_add(len)?)
}
/// 系统默认应用图标。自制图标解析失败时的退路。
fn system_icon() -> Option<HICON> {
    use windows::Win32::UI::WindowsAndMessaging::{IDI_APPLICATION, LoadIconW};

    unsafe { LoadIconW(None, IDI_APPLICATION) }.ok()
}

fn notify_data(hwnd: HWND) -> NOTIFYICONDATAW {
    let mut data = NOTIFYICONDATAW {
        cbSize: std::mem::size_of::<NOTIFYICONDATAW>() as u32,
        hWnd: hwnd,
        uID: ICON_ID,
        uFlags: NIF_ICON | NIF_MESSAGE | NIF_TIP,
        uCallbackMessage: WM_TRAY,
        hIcon: HICON(ICON.load(Ordering::Relaxed) as *mut _),
        ..Default::default()
    };

    // tooltip 只给不带 token 的地址：鼠标划过去就能看到的东西不该含凭据。
    if let Some(ctx) = CTX.get() {
        // 带上版本号：双击启动的用户看不到横幅，这是他们唯一能看到版本的地方，
        // 而更新之后「到底换没换成」正是要确认的事。
        let tip: Vec<u16> = format!("cross-next {}  {}", ui::VERSION, ctx.bare)
            .encode_utf16()
            .collect();
        // szTip 要以 NUL 结尾，所以最多只填 len-1 个字符。
        let n = tip.len().min(data.szTip.len() - 1);
        data.szTip[..n].copy_from_slice(&tip[..n]);
    }

    data
}

fn add_icon(hwnd: HWND) {
    let data = notify_data(hwnd);
    if !unsafe { Shell_NotifyIconW(NIM_ADD, &data) }.as_bool() {
        // 加不上就只是没图标，服务端照常工作。打到控制台（若有）即可，不弹框。
        eprintln!("提示：托盘图标添加失败");
    }
}

/// 弹右键菜单。
fn show_menu(hwnd: HWND) {
    unsafe {
        let mut pt = POINT::default();
        let _ = GetCursorPos(&mut pt);

        let Ok(menu) = CreatePopupMenu() else { return };

        let _ = AppendMenuW(menu, MF_STRING, CMD_ADDRESS, w!("显示访问地址"));
        let _ = AppendMenuW(menu, MF_STRING, CMD_OPEN, w!("在本机打开页面"));
        let _ = AppendMenuW(menu, MF_SEPARATOR, 0, PCWSTR::null());
        let _ = AppendMenuW(menu, MF_STRING, CMD_QUIT, w!("退出"));

        // TrackPopupMenu 要求调用方是前台窗口，否则菜单在点击别处时不会消失。
        let _ = SetForegroundWindow(hwnd);

        let _ = TrackPopupMenu(menu, TPM_RIGHTBUTTON, pt.x, pt.y, None, hwnd, None);

        // 托盘菜单的老毛病：不补这一下，菜单消失后第一次点击会被吞掉。
        let _ = PostMessageW(Some(hwnd), WM_NULL, WPARAM(0), LPARAM(0));

        let _ = DestroyMenu(menu);
    }
}

/// 把访问地址显示出来，同时塞进剪贴板。
///
/// 这是双击启动时用户拿到 URL 的唯一途径（横幅那时无声），所以顺手复制 ——
/// 让人对着消息框手抄一个 64 位十六进制 token 是不现实的。
fn show_address(hwnd: HWND) {
    let Some(ctx) = CTX.get() else { return };

    let copied = copy_to_clipboard(hwnd, &ctx.url);
    let msg = format!(
        "在同一局域网的浏览器里打开：\n\n{}\n\n{}",
        ctx.url,
        if copied {
            "（已复制到剪贴板）"
        } else {
            "（复制到剪贴板失败，请手动记录）"
        }
    );

    // 用户是在图形界面上点的菜单，回答就该出现在图形界面上 —— 即使此时恰好
    // 还连着一个控制台。
    ui::box_message(TITLE, &msg, false);
}

fn copy_to_clipboard(hwnd: HWND, text: &str) -> bool {
    use windows::Win32::Foundation::{GlobalFree, HANDLE};
    use windows::Win32::System::DataExchange::{
        CloseClipboard, EmptyClipboard, OpenClipboard, SetClipboardData,
    };
    use windows::Win32::System::Memory::{GMEM_MOVEABLE, GlobalAlloc, GlobalLock, GlobalUnlock};

    let wide: Vec<u16> = text.encode_utf16().chain(std::iter::once(0)).collect();
    let bytes = std::mem::size_of_val(wide.as_slice());

    unsafe {
        if OpenClipboard(Some(hwnd)).is_err() {
            return false;
        }

        // 提前拿到关闭这个作用域的办法：中途任何一步失败都必须 CloseClipboard，
        // 否则剪贴板会被本进程一直锁着，别的程序复制粘贴全部失灵。
        let result = (|| {
            EmptyClipboard().ok()?;

            let h = GlobalAlloc(GMEM_MOVEABLE, bytes).ok()?;
            let p = GlobalLock(h);
            if p.is_null() {
                let _ = GlobalFree(Some(h));
                return None;
            }
            std::ptr::copy_nonoverlapping(wide.as_ptr(), p as *mut u16, wide.len());
            let _ = GlobalUnlock(h);

            // 13 = CF_UNICODETEXT。用字面量是为了不给 windows crate 多开
            // 一个 Win32_System_Ole feature —— 这个函数收的本就是 u32。
            //
            // 成功后内存所有权归剪贴板，不能再 GlobalFree；失败才要自己收。
            if SetClipboardData(13, Some(HANDLE(h.0))).is_err() {
                let _ = GlobalFree(Some(h));
                return None;
            }
            Some(())
        })();

        let _ = CloseClipboard();
        result.is_some()
    }
}

/// 用默认浏览器打开页面。给在本机核验用。
fn open_page() {
    use windows::Win32::UI::Shell::ShellExecuteW;
    use windows::core::HSTRING;

    let Some(ctx) = CTX.get() else { return };
    let url = HSTRING::from(ctx.url.as_str());
    unsafe {
        ShellExecuteW(
            None,
            w!("open"),
            &url,
            PCWSTR::null(),
            PCWSTR::null(),
            SW_SHOWNORMAL,
        );
    }
}

unsafe extern "system" fn wndproc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    // 注册消息的消息号在运行时才定，进不了 match 的模式位置。
    if msg != 0 && msg == TASKBAR_CREATED.load(Ordering::Relaxed) {
        add_icon(hwnd);
        return LRESULT(0);
    }

    match msg {
        // 托盘图标事件。经典（非 version 4）回调约定：wParam 是图标 id，
        // lParam 是鼠标消息。够用，也省掉 version 4 那套坐标换算。
        WM_TRAY => {
            match lparam.0 as u32 {
                WM_RBUTTONUP => show_menu(hwnd),
                // 双击图标直接开页面，省一次右键。
                WM_LBUTTONDBLCLK => open_page(),
                _ => {}
            }
            LRESULT(0)
        }

        WM_COMMAND => {
            // 低 16 位是菜单项 id，高 16 位是通知码，这里用不上。
            match wparam.0 & 0xffff {
                CMD_ADDRESS => show_address(hwnd),
                CMD_OPEN => open_page(),
                CMD_QUIT => {
                    let _ = unsafe { DestroyWindow(hwnd) };
                }
                _ => {}
            }
            LRESULT(0)
        }

        WM_DESTROY => {
            // 必须在进程结束前摘掉图标，否则托盘留一个幽灵。
            let data = NOTIFYICONDATAW {
                cbSize: std::mem::size_of::<NOTIFYICONDATAW>() as u32,
                hWnd: hwnd,
                uID: ICON_ID,
                ..Default::default()
            };
            let _ = unsafe { Shell_NotifyIconW(NIM_DELETE, &data) };
            unsafe { PostQuitMessage(0) };
            LRESULT(0)
        }

        _ => unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 内嵌的图标必须是能解出图来的 —— 换图标或改生成脚本时这条会兜住。
    #[test]
    fn embedded_icon_parses() {
        for want in [16, 20, 24, 32, 48] {
            let bits = pick_image(ICON_BYTES, want).expect("应能选出一张图");
            // BITMAPINFOHEADER 的 biSize 恒为 40，用它确认切的位置对齐到了图头
            // 而不是文件中间某处。
            assert_eq!(u32::from_le_bytes(bits[0..4].try_into().unwrap()), 40);
            // biHeight 是 XOR + AND 两块之和，所以应是宽的两倍。
            let w = i32::from_le_bytes(bits[4..8].try_into().unwrap());
            let h = i32::from_le_bytes(bits[8..12].try_into().unwrap());
            assert_eq!(h, w * 2, "{want}px 这张的高度字段不对");
        }
    }

    /// 16 和 32 两档要真的分得开，别都落到同一张上。
    #[test]
    fn picks_nearest_size() {
        let small = pick_image(ICON_BYTES, 16).unwrap();
        let large = pick_image(ICON_BYTES, 32).unwrap();
        assert_eq!(i32::from_le_bytes(small[4..8].try_into().unwrap()), 16);
        assert_eq!(i32::from_le_bytes(large[4..8].try_into().unwrap()), 32);
    }

    /// 畸形输入返回 None，不 panic —— 越界读会把整个进程带走。
    #[test]
    fn malformed_is_none() {
        assert!(pick_image(&[], 16).is_none());
        assert!(pick_image(&[0, 0, 1, 0], 16).is_none()); // 头被截断
        // 声称有一张图，但目录项不全
        assert!(pick_image(&[0, 0, 1, 0, 1, 0, 16, 16], 16).is_none());
        // 目录项完整但偏移越界
        let mut bad = vec![0, 0, 1, 0, 1, 0];
        bad.extend_from_slice(&[16, 16, 0, 0, 1, 0, 32, 0]);
        bad.extend_from_slice(&999u32.to_le_bytes()); // 长度
        bad.extend_from_slice(&999u32.to_le_bytes()); // 偏移
        assert!(pick_image(&bad, 16).is_none());
    }
}
