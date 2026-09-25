//! `remote.exe` —— 一次性发一条命令给 cross-next，然后退出。
//!
//! 这是给鼠标/键盘驱动用的：把侧键绑到「启动程序」，参数填 next 或 prev。
//! 按一下就切一首歌，不需要常驻进程，也不需要键盘钩子 —— 驱动到底发什么键码
//! 这个不确定性直接绕过了。
//!
//!   remote.exe next
//!   remote.exe prev
//!   remote.exe playpause
//!   remote.exe restart      跳回当前曲目开头
//!   remote.exe replay       跳回开头并开始播放
//!   remote.exe vol +10      音量相对调整（百分点）
//!   remote.exe vol 60       音量设为 60%
//!   remote.exe mute
//!
//! 服务器地址与 token 从同目录的 remote.json 读，没有就按提示生成一份。
//!
//! 编译成 Windows 子系统（见 main 上方的 windows_subsystem 属性），所以按下去
//! 不会闪一个黑框。出错时用消息框提示，因为这种场景下没有控制台可看。

#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use cross_next::{client, ui};

/// 消息框标题。
const TITLE: &str = "cross-next remote";

fn main() {
    ui::init_dpi();

    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.is_empty() {
        ui::fail(TITLE, &usage());
        return;
    }

    // 放在 dispatch 之前，不然 --version 会被当成一条命令发给服务端。
    if args.iter().any(|a| a == "--version" || a == "-V") {
        ui::report(TITLE, &format!("cross-next remote {}", ui::VERSION));
        return;
    }

    let config = match client::load_config() {
        Ok(c) => c,
        Err(e) => {
            ui::fail(TITLE, &e);
            return;
        }
    };

    if let Err(e) = client::dispatch(&config, &args) {
        ui::fail(TITLE, &format!("{e}\n\n{}", usage()));
    }
}

fn usage() -> String {
    "用法：

       remote.exe next
       remote.exe prev
       remote.exe playpause
       remote.exe restart     跳回当前曲目开头
       remote.exe replay      跳回开头并开始播放
       remote.exe vol +10     音量加 10 个百分点
       remote.exe vol 60      音量设为 60%
       remote.exe mute        静音开关

     驱动没有「启动程序」选项时，改用 listen.exe（常驻，抢媒体键转发）。
"
    .to_string()
}
