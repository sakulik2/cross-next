//! 播放期间阻止系统休眠。
//!
//! 条件刻意选「歌在放」而不是「cross-next 在跑」—— 后者等于让一个遥控服务把机器
//! 永久钉醒，那不是遥控器该干的事。播放一停就放开，系统照常按自己的电源策略睡。
//!
//! 只要 `ES_SYSTEM_REQUIRED`，不要 `ES_DISPLAY_REQUIRED`：听歌不需要亮屏，屏幕该
//! 熄就熄。
//!
//! `SetThreadExecutionState` 是**按线程**生效的，线程退出时状态自动释放。所以这个
//! 守卫必须住在一个长命线程里 —— 媒体线程正合适，它活整个进程生命周期，而且本来
//! 就知道播放状态。

use windows::Win32::System::Power::{
    ES_CONTINUOUS, ES_SYSTEM_REQUIRED, EXECUTION_STATE, SetThreadExecutionState,
};

/// 休眠抑制守卫。只在状态真正翻转时调用系统 API，而不是每次轮询都调一遍。
#[derive(Default)]
pub struct KeepAwake {
    held: bool,
}

impl KeepAwake {
    pub fn new() -> Self {
        Self::default()
    }

    /// 按当前是否在播放更新抑制状态。幂等，可以每秒调。
    pub fn set(&mut self, playing: bool) {
        if playing == self.held {
            return;
        }

        // ES_CONTINUOUS 表示「持续生效直到下次改变」，而不是只顶一次。
        // 单独传 ES_CONTINUOUS（不带 ES_SYSTEM_REQUIRED）即解除抑制。
        let flags = if playing {
            ES_CONTINUOUS | ES_SYSTEM_REQUIRED
        } else {
            ES_CONTINUOUS
        };

        // 返回 0 表示失败。失败不影响播放控制，所以只记不断 —— 最坏情况是机器
        // 照常休眠，用户会发现，比让整个服务挂掉好。
        if unsafe { SetThreadExecutionState(flags) } == EXECUTION_STATE(0) {
            eprintln!("提示：设置休眠抑制失败，播放时系统可能仍会休眠");
            return;
        }

        self.held = playing;
    }
}

impl Drop for KeepAwake {
    fn drop(&mut self) {
        // 线程退出本来就会释放，但显式解除更清楚，也覆盖守卫早于线程销毁的情形。
        if self.held {
            unsafe { SetThreadExecutionState(ES_CONTINUOUS) };
        }
    }
}
