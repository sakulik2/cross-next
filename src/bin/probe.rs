//! 实机探针：把 SMTC 会话与音频会话的真实取值打印出来。
//!
//! 用途是确认 QQ音乐 的 `SourceAppUserModelId` 和进程名到底是什么 —— 这两个值
//! 不该靠猜，填进 config.json 的 `target` 之前先用这个程序照一眼。
//!
//! 注意：必须在交互桌面会话里运行。SMTC 会话按 Windows 登录会话隔离，
//! 从 SSH 之类的非交互会话跑，GetSessions() 会返回空，Win11 上
//! RequestAsync() 甚至直接抛 0x80070424。

use windows::Media::Control::GlobalSystemMediaTransportControlsSessionManager as SessionManager;
// windows-future 的 Async trait 是私有的，没有公开的阻塞 get()，自己写了一个。
use cross_next::winrt_block::block_on;
use windows::Win32::Media::Audio::{
    IAudioSessionControl2, IAudioSessionManager2, IMMDeviceEnumerator, MMDeviceEnumerator,
    eMultimedia, eRender,
};
use windows::Win32::System::Com::{
    CLSCTX_ALL, COINIT_MULTITHREADED, CoCreateInstance, CoInitializeEx,
};
use windows::core::Interface; // IAudioSessionControl -> IAudioSessionControl2 的 cast
use windows::core::Result;

fn main() {
    println!("cross-next 探针\n");

    // 会话隔离是「一个 SMTC 会话都没有」最常见的成因，先判定它。
    diagnose_session();

    // SMTC 与 Core Audio 都要 COM。MTA 单元，与主程序的媒体线程保持一致。
    unsafe {
        // 已初始化过返回 S_FALSE，不是错误，所以只记不断。
        let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
    }

    match probe_smtc() {
        Ok(0) => {
            println!("  未发现任何 SMTC 会话 —— 一个都没有，不只是缺 QQ音乐。");
            println!();
            println!("  先看上面的「运行环境」：如果那里报了问题，先解决它，");
            println!("  下面几条都不用查。");
            println!();
            println!("  否则按这个顺序排查：");
            println!("    1. 用浏览器放一个视频，再跑一次这个探针。");
            println!("       如果浏览器出现在列表里 → SMTC 正常，是 QQ音乐 没注册；");
            println!("       如果还是空的 → 是系统层面的问题，与 QQ音乐 无关。");
            println!("    2. QQ音乐 是否真的在播放（暂停也算，但从没播过则不注册会话）。");
            println!("    3. 试试键盘或耳机上的媒体键能不能控制 QQ音乐。");
            println!("       能 → 它走的是媒体键而非 SMTC，需要换控制方式；");
            println!("       不能 → 它可能没接入系统媒体框架。");
            println!("    4. 新版 QQ音乐 已移除 SMTC 开关（默认常开），找不到那个设置项是正常的。");
        }
        Ok(_) => {}
        Err(e) => {
            println!("  SMTC 读取失败: {e}");
            if e.code().0 as u32 == 0x8007_0424 {
                println!("  0x80070424 表示当前是非交互会话（SSH / 服务）。");
                println!("  必须在登录的桌面会话里运行。");
            }
        }
    }

    println!();

    if let Err(e) = probe_audio() {
        println!("  音频会话读取失败: {e}");
    }

    // 双击运行时控制台会随进程退出一起关掉，什么都看不到。等一下回车。
    println!("\n按回车退出。");
    let _ = std::io::stdin().read_line(&mut String::new());
}

/// 判定当前进程是否跑在交互桌面会话里。
///
/// SMTC 会话按 Windows 登录会话隔离。如果本进程的会话号与活动控制台会话号不一致，
/// 那就无论如何都看不到桌面上播放器注册的会话 —— 这时候查 QQ音乐 的版本和设置
/// 都是白费功夫，得先把启动方式改对。
fn diagnose_session() {
    use windows::Win32::System::RemoteDesktop::{
        ProcessIdToSessionId, WTSGetActiveConsoleSessionId,
    };
    use windows::Win32::System::Threading::GetCurrentProcessId;

    println!("== 运行环境 ==");

    unsafe {
        let pid = GetCurrentProcessId();
        let mut mine = 0u32;
        if ProcessIdToSessionId(pid, &mut mine).is_err() {
            println!("  无法取得本进程的会话号");
            println!();
            return;
        }
        let console = WTSGetActiveConsoleSessionId();

        println!("  本进程会话: {mine}   活动桌面会话: {console}");

        if mine == 0 {
            println!("  [问题] 会话 0 是服务专用的非交互会话。");
            println!("         SMTC 在这里永远看不到任何播放器。");
            println!("         不要做成 Windows 服务，也不要用 psexec -s 之类启动。");
        } else if mine != console {
            println!("  [问题] 本进程不在活动桌面会话里（可能来自 SSH 或远程会话）。");
            println!("         SMTC 会话按登录会话隔离，跨会话读不到。");
            println!("         请在台式机自己的桌面上双击运行。");
        } else {
            println!("  交互桌面会话，SMTC 可用。");
        }
    }

    println!();
}

/// 打印所有 SMTC 会话，返回会话数量。
fn probe_smtc() -> Result<usize> {
    println!("== SMTC 会话 ==");

    let manager = block_on(SessionManager::RequestAsync()?)?;
    let sessions = manager.GetSessions()?;
    let count = sessions.Size()? as usize;

    // GetCurrentSession 在无会话时返回 Err 而非 None，所以用 ok() 吞掉。
    let current_id = manager
        .GetCurrentSession()
        .ok()
        .and_then(|s| s.SourceAppUserModelId().ok())
        .map(|s| s.to_string());

    for session in sessions {
        let aumid = session.SourceAppUserModelId()?.to_string();
        let is_current = current_id.as_deref() == Some(aumid.as_str());

        println!(
            "\n  AUMID: {aumid}{}",
            if is_current { "   <- 当前会话" } else { "" }
        );

        // 元数据取不到不该中断整个枚举，逐项降级。
        match session.TryGetMediaPropertiesAsync().and_then(block_on) {
            Ok(props) => {
                let title = props.Title().map(|s| s.to_string()).unwrap_or_default();
                let artist = props.Artist().map(|s| s.to_string()).unwrap_or_default();
                let album = props
                    .AlbumTitle()
                    .map(|s| s.to_string())
                    .unwrap_or_default();
                println!("    曲目: {title} / {artist} / {album}");
                println!(
                    "    封面: {}",
                    if props.Thumbnail().is_ok() {
                        "有"
                    } else {
                        "无"
                    }
                );
            }
            Err(e) => println!("    曲目: <读取失败 {e}>"),
        }

        if let Ok(info) = session.GetPlaybackInfo() {
            if let Ok(status) = info.PlaybackStatus() {
                println!("    状态: {status:?}");
            }
            if let Ok(c) = info.Controls() {
                println!(
                    "    可用: 上一首={} 下一首={} 播放={} 暂停={} 定位={}",
                    c.IsPreviousEnabled().unwrap_or(false),
                    c.IsNextEnabled().unwrap_or(false),
                    c.IsPlayEnabled().unwrap_or(false),
                    c.IsPauseEnabled().unwrap_or(false),
                    c.IsPlaybackPositionEnabled().unwrap_or(false),
                );
            }
        }

        // 时间轴是本项目最不确定的一块：QQ音乐 低于 21.10.2962 不上报，
        // 此时 EndTime 为 0，前端需降级隐藏进度条而不是显示错误数据。
        if let Ok(t) = session.GetTimelineProperties() {
            let pos = t.Position().map(|d| d.Duration).unwrap_or(0);
            let end = t.EndTime().map(|d| d.Duration).unwrap_or(0);
            println!(
                "    时间轴: {:.1}s / {:.1}s{}",
                pos as f64 / 1e7,
                end as f64 / 1e7,
                if end == 0 {
                    "   <- 不上报时间轴，进度条需降级隐藏"
                } else {
                    ""
                }
            );
        }
    }

    println!("\n  共 {count} 个会话");
    Ok(count)
}

/// 打印默认输出设备上的所有音频会话及其进程名。
fn probe_audio() -> Result<()> {
    println!("== 音频会话（分应用音量）==");

    unsafe {
        let enumerator: IMMDeviceEnumerator =
            CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL)?;
        let device = enumerator.GetDefaultAudioEndpoint(eRender, eMultimedia)?;
        let manager: IAudioSessionManager2 = device.Activate(CLSCTX_ALL, None)?;
        let sessions = manager.GetSessionEnumerator()?;

        let count = sessions.GetCount()?;
        for i in 0..count {
            let ctrl = sessions.GetSession(i)?;
            let ctrl2: IAudioSessionControl2 = ctrl.cast()?;

            let pid = ctrl2.GetProcessId().unwrap_or(0);
            let name = cross_next_probe_process_name(pid);

            // 同一个应用可能占多个音频会话（浏览器分标签是典型），
            // 所以主程序设置音量时要对所有匹配项都设置，不能只改第一个。
            println!("  pid={pid:<6} 进程={name}");
        }

        println!("\n  共 {count} 个音频会话");
    }

    Ok(())
}

/// PID 反查进程名。Core Audio 只认 PID，匹配进程名得自己做。
///
/// 与 `volume.rs` 保持一致：用 `QueryFullProcessImageNameW`，它只要
/// `PROCESS_QUERY_LIMITED_INFORMATION`。换成 `GetModuleBaseNameW` 会额外需要
/// `PROCESS_VM_READ`，对以管理员身份运行的进程会失败 —— 那样探针报"无法打开进程"，
/// 而主程序其实也查不到，排查时容易误判成别的原因。
fn cross_next_probe_process_name(pid: u32) -> String {
    use windows::Win32::System::Threading::{
        OpenProcess, PROCESS_NAME_WIN32, PROCESS_QUERY_LIMITED_INFORMATION,
        QueryFullProcessImageNameW,
    };
    use windows::core::PWSTR;

    if pid == 0 {
        return "<系统混音>".into();
    }

    unsafe {
        let Ok(handle) = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid) else {
            return "<无法打开进程>".into();
        };

        let mut buf = [0u16; 260];
        let mut len = buf.len() as u32;
        let ok = QueryFullProcessImageNameW(
            handle,
            PROCESS_NAME_WIN32,
            PWSTR(buf.as_mut_ptr()),
            &mut len,
        );
        let _ = windows::Win32::Foundation::CloseHandle(handle);

        if ok.is_err() || len == 0 {
            return "<未知>".into();
        }

        // 只取文件名，与 volume.rs 的匹配口径一致。
        let full = String::from_utf16_lossy(&buf[..len as usize]);
        full.rsplit(['\\', '/']).next().unwrap_or(&full).to_string()
    }
}
