//! 分应用音量：只动 QQ音乐 的音量，不碰系统总音量。
//!
//! SMTC 不含音量，这部分得走 Core Audio。两个实测得出的注意点：
//!
//!   1. Core Audio 只认 PID，不认进程名，所以匹配进程名要自己用 PID 反查。
//!   2. 只有「活跃过」的会话才在枚举里。QQ音乐 没实际出声时列表里根本没有它 ——
//!      所以不能启动时枚举一次就缓存，每次都得重新找，找不到是正常情况。
//!
//! 另外一个应用可能占多个音频会话，设音量时要对所有匹配项都设置。

use windows::Win32::Media::Audio::{
    AudioSessionStateActive, IAudioSessionControl2, IAudioSessionManager2, IMMDeviceEnumerator,
    ISimpleAudioVolume, MMDeviceEnumerator, eMultimedia, eRender,
};
use windows::Win32::System::Com::{CLSCTX_ALL, CoCreateInstance};
use windows::core::{Interface, Result};

pub struct VolumeState {
    pub level: f32,
    pub muted: bool,
    /// 这个会话是否正在真的出声。
    ///
    /// mediakey 模式下没有 SMTC，这是唯一能拿到的播放信号 —— 休眠抑制靠它。
    /// `AudioSessionStateActive` 表示正在渲染音频，暂停是 `Inactive`，
    /// 而 `Expired` 是进程已退出但会话还没回收。
    pub active: bool,
}

/// 读 QQ音乐 的音量。没有对应音频会话时返回 Ok(None)。
pub fn read(target: &str) -> Result<Option<VolumeState>> {
    let mut found: Option<VolumeState> = None;
    for_each_matching(target, |ctrl, vol| {
        let active = unsafe { ctrl.GetState() }.is_ok_and(|s| s == AudioSessionStateActive);
        match &mut found {
            // 多个会话时取第一个的读数作为展示值。
            None => {
                found = Some(VolumeState {
                    level: unsafe { vol.GetMasterVolume() }.unwrap_or(0.0),
                    muted: unsafe { vol.GetMute() }
                        .map(|b| b.as_bool())
                        .unwrap_or(false),
                    active,
                });
            }
            // 但 active 要对所有会话取或：一个应用可能占多个会话，出声的不一定
            // 是枚举里的第一个。任一个在出声就算在放。
            Some(state) => state.active |= active,
        }
        Ok(())
    })?;
    Ok(found)
}

/// 设音量。返回是否至少命中一个会话。
pub fn set_volume(target: &str, level: f32) -> Result<bool> {
    let level = level.clamp(0.0, 1.0);
    let mut hit = false;
    for_each_matching(target, |_, vol| {
        unsafe { vol.SetMasterVolume(level, std::ptr::null()) }?;
        hit = true;
        Ok(())
    })?;
    Ok(hit)
}

/// 设静音。返回是否至少命中一个会话。
pub fn set_mute(target: &str, muted: bool) -> Result<bool> {
    let mut hit = false;
    for_each_matching(target, |_, vol| {
        unsafe { vol.SetMute(muted, std::ptr::null()) }?;
        hit = true;
        Ok(())
    })?;
    Ok(hit)
}

/// 枚举默认输出设备上的音频会话，对进程名匹配 target 的逐个调用 f。
///
/// 匹配规则与 SMTC 那边保持一致：不区分大小写的子串匹配。target 形如
/// `qqmusic`，进程名形如 `QQMusic.exe`，能命中。
///
/// 回调同时拿到 `IAudioSessionControl2`（读会话状态）和 `ISimpleAudioVolume`
/// （读写音量）—— 两个接口是同一个会话的不同视图。
fn for_each_matching<F>(target: &str, mut f: F) -> Result<()>
where
    F: FnMut(&IAudioSessionControl2, &ISimpleAudioVolume) -> Result<()>,
{
    let needle = target.to_lowercase();
    // 去掉可能带的 .exe，让 config 里写 `QQMusic.exe` 或 `qqmusic` 都能匹配。
    let needle = needle.trim_end_matches(".exe");

    // 先收集候选，再决定用哪些 —— 子串匹配可能同时命中多个不同的应用
    // （典型：target 写成 "qq" 时，QQ.exe 和 QQMusic.exe 都会中）。
    // 有精确同名的进程时只认它，避免误调别的应用的音量。
    let mut candidates: Vec<(String, IAudioSessionControl2, ISimpleAudioVolume)> = Vec::new();

    unsafe {
        let enumerator: IMMDeviceEnumerator =
            CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL)?;
        let device = enumerator.GetDefaultAudioEndpoint(eRender, eMultimedia)?;
        let manager: IAudioSessionManager2 = device.Activate(CLSCTX_ALL, None)?;
        let sessions = manager.GetSessionEnumerator()?;

        for i in 0..sessions.GetCount()? {
            let ctrl = sessions.GetSession(i)?;
            let ctrl2: IAudioSessionControl2 = ctrl.cast()?;

            let pid = match ctrl2.GetProcessId() {
                Ok(p) if p != 0 => p,
                _ => continue, // 0 是系统混音，跳过
            };

            let Some(name) = process_name(pid) else {
                continue;
            };
            let stem = name.to_lowercase();
            let stem = stem.trim_end_matches(".exe");
            if !stem.contains(needle) {
                continue;
            }

            let vol: ISimpleAudioVolume = ctrl2.cast()?;
            candidates.push((stem.to_string(), ctrl2, vol));
        }

        // 有精确同名的就只用它们，否则退回全部子串命中项。
        let exact = candidates.iter().any(|(name, _, _)| name == needle);

        for (name, ctrl, vol) in &candidates {
            if exact && name != needle {
                continue;
            }
            // 同一应用可能占多个音频会话（浏览器分标签是典型），全部设置。
            f(ctrl, vol)?;
        }
    }

    Ok(())
}

/// PID 反查进程名。
///
/// 用 `QueryFullProcessImageNameW` 而不是 `GetModuleBaseNameW`：后者要求句柄同时
/// 具备 `PROCESS_QUERY_INFORMATION` 和 `PROCESS_VM_READ`，而前者只要
/// `PROCESS_QUERY_LIMITED_INFORMATION`。权限要求低这一点是实质性的 ——
/// QQ音乐 以管理员身份运行时，普通权限的 cross-next 拿不到 VM_READ，
/// 于是查不出进程名，音量功能整个静默失效。
fn process_name(pid: u32) -> Option<String> {
    use windows::Win32::Foundation::CloseHandle;
    use windows::Win32::System::Threading::{
        OpenProcess, PROCESS_NAME_WIN32, PROCESS_QUERY_LIMITED_INFORMATION,
        QueryFullProcessImageNameW,
    };
    use windows::core::PWSTR;

    unsafe {
        let handle = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid).ok()?;

        // 返回的是完整路径，容量按 MAX_PATH 给足。
        let mut buf = [0u16; 260];
        let mut len = buf.len() as u32;
        let ok = QueryFullProcessImageNameW(
            handle,
            PROCESS_NAME_WIN32,
            PWSTR(buf.as_mut_ptr()),
            &mut len,
        );
        let _ = CloseHandle(handle);

        ok.ok()?;
        if len == 0 {
            return None;
        }

        // 只要文件名部分 —— 调用方拿它和 target 做子串匹配，带上目录会让
        // 安装路径里的字符串也参与匹配（"D:\QQMusic\other.exe" 会误命中）。
        let full = String::from_utf16_lossy(&buf[..len as usize]);
        Some(full.rsplit(['\\', '/']).next().unwrap_or(&full).to_string())
    }
}
