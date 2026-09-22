//! 配置热重载与连通性探活。给 `listen.exe` 用。
//!
//! 两件事共用一个后台线程，因为它们都只需要「等一个事件或超时」：
//!
//!   - **热重载**：`remote.json` 改了就换掉内存里的 Config。事件驱动，不轮询 ——
//!     `FindFirstChangeNotificationW` + `WaitForSingleObject` 带超时，超时那一路
//!     正好用来做探活。
//!   - **探活**：太久没有按键活动时试一次 `/api/state`，确认服务端还在。
//!     按键本身就是一次连通性测试，所以刚按过就不必再探。
//!
//! 刻意**不重启进程**来应用新配置：重启会先释放热键再重新注册，那个窗口期里别的
//! 程序可能抢走。换掉 Mutex 里的 Config 没有这个空隙。
//!
//! 探活连续失败也**不退出进程**。台式机休眠时连不上是必然的，而那恰恰是最需要它
//! 待命的时刻 —— 一旦放弃热键，媒体键就会跑去控制本机播放器，也就是用户最初想
//! 避免的事。失败只是让出热键，等服务端回来再抢回。

use crate::client::{self, Config};
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use windows::Win32::Foundation::{HANDLE, WAIT_OBJECT_0};
use windows::Win32::Storage::FileSystem::{
    FILE_NOTIFY_CHANGE_FILE_NAME, FILE_NOTIFY_CHANGE_LAST_WRITE, FILE_NOTIFY_CHANGE_SIZE,
    FindCloseChangeNotification, FindFirstChangeNotificationW, FindNextChangeNotification,
};
use windows::Win32::System::Threading::WaitForSingleObject;
use windows::core::HSTRING;

/// 空闲多久之后开始探活。按键本身就是连通性测试，刚按过就不必再探。
const IDLE_BEFORE_PROBE: Duration = Duration::from_secs(300);
/// 等待粒度：文件变更通知的超时时长，也是探活的最小间隔。
const TICK: Duration = Duration::from_secs(30);
/// 连续失败多少次算「服务端不可达」，此时让出热键。
const FAILURES_TO_YIELD: u32 = 5;

/// 已让出热键后的重连间隔，退避用。
///
/// 让出热键后，探活是**唯一**的恢复途径 —— 按键已经不经过我们了，不会再产生活动，
/// 所以此时必须忽略空闲门槛主动重试，否则恢复延迟会接近 IDLE_BEFORE_PROBE。
///
/// 从 5 秒起步是为了休眠唤醒：网络栈要几秒才就绪，第一次试大概会失败，但紧接着
/// 就会再试。退避到 60 秒上限，避免台式机长期关机时白打请求。
const RECONNECT_BACKOFF: [u64; 5] = [5, 10, 20, 40, 60];

/// 后台线程与主线程之间的共享状态。
pub struct Shared {
    /// 当前生效的配置。热重载直接换这里，转发线程每次取用。
    pub config: Mutex<Config>,
    /// 最近一次按键活动的时间，用 Instant 的秒数近似表示。
    /// 用原子量而非 Mutex，因为按键路径上不该有锁竞争。
    last_activity: AtomicU64,
    /// 进程启动时刻，作为 last_activity 的基准。
    started: Instant,
    /// 服务端当前是否可达。false 时主线程会让出热键。
    reachable: Mutex<bool>,
}

impl Shared {
    pub fn new(config: Config) -> Arc<Self> {
        Arc::new(Self {
            config: Mutex::new(config),
            last_activity: AtomicU64::new(0),
            started: Instant::now(),
            reachable: Mutex::new(true),
        })
    }

    /// 取一份配置副本。转发线程用，避免长时间持锁。
    pub fn config(&self) -> Config {
        self.config.lock().expect("config mutex poisoned").clone()
    }

    /// 记一次按键活动。按键本身就是连通性测试，所以这会推迟下一次探活。
    pub fn touch(&self) {
        self.last_activity
            .store(self.started.elapsed().as_secs(), Ordering::Relaxed);
    }

    /// 距上次活动过了多久。
    fn idle(&self) -> Duration {
        let last = self.last_activity.load(Ordering::Relaxed);
        self.started
            .elapsed()
            .saturating_sub(Duration::from_secs(last))
    }

    pub fn reachable(&self) -> bool {
        *self.reachable.lock().expect("reachable mutex poisoned")
    }

    fn set_reachable(&self, value: bool) {
        *self.reachable.lock().expect("reachable mutex poisoned") = value;
    }
}

/// 后台线程要通知主线程做的事。
pub enum Event {
    /// 配置变了，已经换好；附带新的目标地址供提示。
    Reloaded(String),
    /// 服务端连不上，让出热键。
    Unreachable,
    /// 服务端回来了，重新抢热键。
    Recovered,
}

/// 起监视线程。`notify` 在有事发生时被调用（在后台线程上，别在里面做重活）。
pub fn spawn<F>(shared: Arc<Shared>, notify: F)
where
    F: Fn(Event) + Send + 'static,
{
    std::thread::Builder::new()
        .name("watch".into())
        .spawn(move || run(shared, notify))
        .expect("监视线程启动失败");
}

fn run<F: Fn(Event)>(shared: Arc<Shared>, notify: F) {
    let path = client::config_path();
    // 监视的是**目录**而非文件 —— 很多编辑器保存时先写临时文件再改名，
    // 直接盯文件会漏掉那种写法。
    let dir = path.parent().map(|p| p.to_path_buf());

    let mut handle = dir.and_then(|d| watch_dir(&d));
    let mut failures = 0u32;

    loop {
        // 等待时长取决于是否处于重连状态：不可达时用退避间隔主动重试，
        // 可达时用 TICK 粒度（探活本身还有空闲门槛把关）。
        let wait = if shared.reachable() {
            TICK
        } else {
            let i = (failures.saturating_sub(FAILURES_TO_YIELD) as usize)
                .min(RECONNECT_BACKOFF.len() - 1);
            Duration::from_secs(RECONNECT_BACKOFF[i])
        };

        // 有变更通知句柄就等它，没有就退化成纯定时器 —— 探活照样要跑。
        match handle {
            Some(h) => {
                let waited = unsafe { WaitForSingleObject(h, wait.as_millis() as u32) };
                if waited == WAIT_OBJECT_0 {
                    // 收到变更。编辑器保存常触发多次，稍等一下再读，
                    // 免得读到写了一半的文件。
                    std::thread::sleep(Duration::from_millis(250));
                    reload(&shared, &notify);
                    // 必须重新武装，否则后续变更收不到。失败就丢掉句柄，
                    // 之后退化为纯定时循环（探活仍然工作）。
                    if unsafe { FindNextChangeNotification(h) }.is_err() {
                        let _ = unsafe { FindCloseChangeNotification(h) };
                        handle = None;
                    }
                    continue;
                }
            }
            None => std::thread::sleep(wait),
        }

        // 走到这里是超时（或没有监视句柄），做探活。
        probe(&shared, &notify, &mut failures);
    }
}

fn watch_dir(dir: &Path) -> Option<HANDLE> {
    let wide = HSTRING::from(dir.as_os_str());
    // 同时关注写入、改名和大小变化，覆盖「直接写」和「临时文件+改名」两种保存方式。
    let filter =
        FILE_NOTIFY_CHANGE_FILE_NAME | FILE_NOTIFY_CHANGE_LAST_WRITE | FILE_NOTIFY_CHANGE_SIZE;
    unsafe { FindFirstChangeNotificationW(&wide, false, filter) }.ok()
}

/// 重新读配置。只在内容真的变了时才替换并通知。
fn reload<F: Fn(Event)>(shared: &Arc<Shared>, notify: &F) {
    let Ok(fresh) = client::load_config() else {
        // 读失败（文件被改坏、token 被清空）时保留旧配置继续工作，
        // 不要因为一次误编辑就让遥控器失效。
        return;
    };

    let mut guard = shared.config.lock().expect("config mutex poisoned");
    if *guard == fresh {
        return; // 同一个目录下别的文件变了，或者内容没实质变化
    }

    let target = format!("{}:{}", fresh.host, fresh.port);
    *guard = fresh;
    drop(guard);

    notify(Event::Reloaded(target));
}

/// 探活。
///
/// 两种模式：可达时只在空闲够久才探（按键已经证明连得上，不必重复验证）；
/// 已让出热键时**忽略空闲门槛**，因为按键不再经过我们、不会产生活动，
/// 探活成了唯一的恢复途径。
fn probe<F: Fn(Event)>(shared: &Arc<Shared>, notify: &F, failures: &mut u32) {
    if shared.reachable() && shared.idle() < IDLE_BEFORE_PROBE {
        return;
    }

    let config = shared.config();
    let ok = client::get(&config, "/api/state").is_ok();

    if ok {
        *failures = 0;
        if !shared.reachable() {
            shared.set_reachable(true);
            // 恢复后把活动时间推后一格，免得刚抢回热键就又立刻探一次。
            shared.touch();
            notify(Event::Recovered);
        }
        return;
    }

    *failures += 1;
    // 达到阈值才让出热键。单次失败可能只是台式机正在睡或网络抖动。
    if *failures >= FAILURES_TO_YIELD && shared.reachable() {
        shared.set_reachable(false);
        notify(Event::Unreachable);
    }
}
