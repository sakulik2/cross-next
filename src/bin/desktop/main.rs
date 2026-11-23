//! `desktop.exe` —— 桌面小组件 + 任务栏缩略图按钮 + 全局组合键。
//!
//! 它只是又一个界面，和网页平级：通过同一套 HTTP API 遥控服务端，服务端不为它
//! 做任何特殊处理。连接配置与服务端、`remote.exe`/`listen.exe` 共用 `config.json`。
//!
//! 三种操作途径，对应打游戏时的三种情形：
//!
//! - **小组件**：置顶的小窗口，点它不抢焦点。无边框窗口模式的游戏、或开在副屏上时用。
//! - **任务栏缩略图按钮**：悬停任务栏图标出现的 ⏮ ⏯ ⏭，和 Spotify 一样。
//! - **全局组合键**：默认 Ctrl+Alt+←/→/空格/↑/↓。独占全屏的游戏里前两种都看不见，
//!   只有这个不用切出去。
//!
//!   desktop.exe            启动（已在运行则把窗口叫出来）
//!   desktop.exe --stop     关掉正在运行的实例
//!   desktop.exe --version

#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod decode;
mod hotkey;
mod model;
mod pixels;
mod settings;
mod taskbar;
mod window;
mod worker;

use cross_next::{client, ui};
use windows::Win32::System::Com::{COINIT_APARTMENTTHREADED, CoInitializeEx};

const TITLE: &str = "cross-next desktop";

fn main() {
    // 必须在创建任何窗口之前。
    ui::init_dpi();

    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.iter().any(|a| a == "--version" || a == "-V") {
        ui::report(TITLE, &format!("cross-next desktop {}", ui::VERSION));
        return;
    }
    if args.iter().any(|a| a == "--stop") {
        let msg = if window::stop_running() {
            "已关闭正在运行的 desktop.exe。"
        } else {
            "没有正在运行的 desktop.exe。"
        };
        ui::report(TITLE, msg);
        return;
    }

    // 它有窗口和任务栏按钮，用户关得掉，所以不学 listen 去接管旧实例 ——
    // 重复启动就是想看到它，把已有的窗口叫出来即可。
    if window::activate_existing() {
        return;
    }

    // 配置在启动时就校验，连不上服务端倒不要紧（界面会显示并自动重连），
    // 但没有 config.json 就不知道连谁。
    let config = match client::load_config() {
        Ok(c) => c,
        Err(e) => {
            ui::fail(TITLE, &e);
            return;
        }
    };
    let settings = settings::load();

    // 任务栏按钮（ITaskbarList3）要 COM，且必须在 UI 线程的 STA 里。
    let _ = unsafe { CoInitializeEx(None, COINIT_APARTMENTTHREADED) }.ok();

    if let Err(e) = window::run(config, settings) {
        ui::fail(TITLE, &e);
    }
}
