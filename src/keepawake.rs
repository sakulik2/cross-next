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
//!
//! **抑制立刻加，松手要延迟。** 「没在放」有大量瞬时假象：换歌加载时 QQ音乐 会短暂
//! 报 `Changing`/`Opened`/`Stopped`，网络卡顿同理，SMTC 会话本身也可能在换歌那一瞬
//! 短暂消失（`find_session` 返回 None，快照整个降级成默认值）。而 Windows 11 取消了
//! 旧版「释放电源请求后再宽限 2 分钟」的行为，请求一松系统立刻可睡；遥控场景下台机
//! 没人碰键鼠，空闲计时器早已到期。两件事叠起来的后果是：任何一次误判都足以让机器
//! **当场**睡过去，而不是只丢掉一个轮询间隔的抑制。所以松手前要先确认它真的连续
//! `GRACE` 这么久都没在放。

use std::time::{Duration, Instant};

use windows::Win32::System::Power::{
    ES_CONTINUOUS, ES_SYSTEM_REQUIRED, EXECUTION_STATE, SetThreadExecutionState,
};

/// 观测到「没在放」之后，还要继续抑制多久才真的松手。
///
/// 只要比最长的换歌/缓冲空档长就够，且宁长勿短 —— 长了不过是真暂停后机器多醒一会儿，
/// 短了就是歌单放到一半机器睡了。90 秒足以盖住换歌加载、网络卡顿，以及 SMTC 会话在
/// 切曲瞬间的短暂消失。
const GRACE: Duration = Duration::from_secs(90);

/// 休眠抑制守卫。只在状态真正翻转时调用系统 API，而不是每次轮询都调一遍。
#[derive(Default)]
pub struct KeepAwake {
    held: bool,
    grace: Grace,
    /// 系统调用失败只警告一次。前端 1 秒一轮，每次都打会把控制台刷满。
    warned: bool,
}

impl KeepAwake {
    pub fn new() -> Self {
        Self::default()
    }

    /// 按当前是否在播放更新抑制状态。幂等，可以每秒调。
    pub fn set(&mut self, playing: bool) {
        let want = self.grace.want(self.held, playing, Instant::now());
        if want == self.held {
            return;
        }

        // ES_CONTINUOUS 表示「持续生效直到下次改变」，而不是只顶一次。
        // 单独传 ES_CONTINUOUS（不带 ES_SYSTEM_REQUIRED）即解除抑制。
        let flags = if want {
            ES_CONTINUOUS | ES_SYSTEM_REQUIRED
        } else {
            ES_CONTINUOUS
        };

        // 返回 0 表示失败。失败不影响播放控制，所以只记不断 —— 最坏情况是机器
        // 照常休眠，用户会发现，比让整个服务挂掉好。
        // 不更新 held，下一轮还会再试。
        if unsafe { SetThreadExecutionState(flags) } == EXECUTION_STATE(0) {
            if !self.warned {
                self.warned = true;
                eprintln!("提示：设置休眠抑制失败，播放时系统可能仍会休眠");
            }
            return;
        }

        self.held = want;
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

/// 松手宽限期的状态机。
///
/// 刻意和 `SetThreadExecutionState` 分开：系统调用在 CI 上没法验（见 CLAUDE.md），
/// 而「换歌空档不该松手」恰恰是这里最容易写错、也最该有测试盯着的一段。
#[derive(Default)]
struct Grace {
    /// 首次观测到「没在放」的时刻。None 表示当前认为在放，或本来就没抑制。
    idle_since: Option<Instant>,
}

impl Grace {
    /// 此刻是否应该持有抑制。`held` 是当前实际状态。
    fn want(&mut self, held: bool, playing: bool, now: Instant) -> bool {
        if playing {
            self.idle_since = None;
            return true;
        }

        // 本来就没抑制就不适用宽限期，否则服务刚启动、QQ音乐 还没开的时候
        // 会凭空把机器钉醒 GRACE 那么久。宽限期只保护已经在放的那首歌。
        if !held {
            self.idle_since = None;
            return false;
        }

        let since = *self.idle_since.get_or_insert(now);
        now.duration_since(since) < GRACE
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 构造一个相对起点偏移若干秒的时刻。
    fn at(base: Instant, secs: u64) -> Instant {
        base + Duration::from_secs(secs)
    }

    #[test]
    fn 启动时没在放不应抑制() {
        let t0 = Instant::now();
        let mut g = Grace::default();
        // 没持有过就不该被宽限期拖着。否则双击启动服务、还没开 QQ音乐，
        // 机器就先被钉醒一分半。
        assert!(!g.want(false, false, t0));
        assert!(!g.want(false, false, at(t0, 200)));
    }

    #[test]
    fn 在放就抑制() {
        let t0 = Instant::now();
        let mut g = Grace::default();
        assert!(g.want(false, true, t0));
    }

    #[test]
    fn 换歌空档不该松手() {
        let t0 = Instant::now();
        let mut g = Grace::default();

        // 正在放，抑制建立。
        assert!(g.want(false, true, t0));
        // 换歌加载：QQ音乐 短暂不报 Playing。这是本次修复的核心场景 ——
        // 立刻松手会让机器当场睡过去。
        assert!(g.want(true, false, at(t0, 1)));
        assert!(g.want(true, false, at(t0, 5)));
        assert!(g.want(true, false, at(t0, 89)));
    }

    #[test]
    fn 恢复播放要重置宽限计时() {
        let t0 = Instant::now();
        let mut g = Grace::default();

        assert!(g.want(false, true, t0));
        // 卡了 60 秒。
        assert!(g.want(true, false, at(t0, 60)));
        // 又放起来了，计时归零。
        assert!(g.want(true, true, at(t0, 61)));
        // 再次中断，此时距上一次中断起点已 80 秒，但距这一次只有 10 秒，
        // 不能因为累计时间够了就松手。
        assert!(g.want(true, false, at(t0, 70)));
        assert!(g.want(true, false, at(t0, 141)));
    }

    #[test]
    fn 真暂停超过宽限期才松手() {
        let t0 = Instant::now();
        let mut g = Grace::default();

        assert!(g.want(false, true, t0));
        assert!(g.want(true, false, at(t0, 10)));
        // 从首次观测到没在放算起满 GRACE，放开，系统照常按自己的策略睡。
        assert!(!g.want(true, false, at(t0, 10 + GRACE.as_secs())));
    }

    #[test]
    fn 松手之后还能再走一轮() {
        let t0 = Instant::now();
        let mut g = Grace::default();

        assert!(g.want(false, true, t0));
        // 宽限计时从首次观测到「没在放」起算，所以要先有这一次把计时点上。
        assert!(g.want(true, false, at(t0, 1)));
        assert!(!g.want(true, false, at(t0, 100)));
        // 松手后 held 变 false，这一轮应清掉计时，不能让残留的 idle_since
        // 把下一首歌的宽限期一开始就算成已过期。
        assert!(!g.want(false, false, at(t0, 101)));
        assert!(g.want(false, true, at(t0, 102)));
        assert!(g.want(true, false, at(t0, 103)));
        assert!(g.want(true, false, at(t0, 190)));
    }
}
