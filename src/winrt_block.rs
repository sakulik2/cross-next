//! WinRT 异步操作的阻塞求值。
//!
//! windows-future 0.3 只提供 `IntoFuture`，那个能把 `IAsyncOperation` 变成 Rust
//! future，但需要一个 async 运行时来驱动。本项目的媒体线程本来就是同步的 ——
//! 它的全部工作就是收命令、调 WinRT、回结果 —— 为此引入 tokio 不划算。
//!
//! 所以这里手写 `SetCompleted` + `Condvar` 等待。这也是 windows-future 内部私有
//! `Async` trait 的做法，只是它没有把阻塞版本公开出来。

use std::sync::{Arc, Condvar, Mutex};
use windows::core::RuntimeType;
use windows::core::{Error, Result};
use windows_future::{
    AsyncOperationCompletedHandler, AsyncOperationWithProgressCompletedHandler, AsyncStatus,
    IAsyncOperation, IAsyncOperationWithProgress,
};

/// E_ABORT：操作被取消。
const E_ABORT: i32 = 0x8000_4004u32 as i32;

/// 等待完成信号。`None` 表示尚未完成。
type Signal = Arc<(Mutex<Option<AsyncStatus>>, Condvar)>;

fn new_signal() -> Signal {
    Arc::new((Mutex::new(None), Condvar::new()))
}

/// 阻塞直到信号置位，取出状态。
fn wait(signal: &Signal) -> AsyncStatus {
    let (lock, cv) = &**signal;
    let mut guard = lock.lock().expect("completion mutex poisoned");
    while guard.is_none() {
        guard = cv.wait(guard).expect("completion mutex poisoned");
    }
    guard.take().expect("signalled without status")
}

/// 回调里置位并唤醒等待方。
fn signal(target: &Signal, status: AsyncStatus) {
    let (lock, cv) = &**target;
    // 只写一个 Option，不会 panic，所以不可能 poison。
    *lock.lock().expect("completion mutex poisoned") = Some(status);
    cv.notify_all();
}

/// 阻塞等待一个 `IAsyncOperation` 完成并取出结果。
///
/// 必须在 MTA 单元的线程上调用。STA 线程上阻塞等待会死锁 —— 回调需要消息泵来
/// 投递，而我们正把这个线程卡住。主程序的媒体线程以 `COINIT_MULTITHREADED`
/// 初始化，满足这个前提。
pub fn block_on<T>(op: IAsyncOperation<T>) -> Result<T>
where
    T: RuntimeType + 'static,
{
    let state = new_signal();
    let sink = Arc::clone(&state);

    op.SetCompleted(&AsyncOperationCompletedHandler::new(
        move |_sender, status| {
            signal(&sink, status);
            Ok(())
        },
    ))?;

    // 操作可能在 SetCompleted 返回前就完成，此时回调已同步跑过、Option 已是
    // Some，wait 立刻返回，不会漏掉通知。
    match wait(&state) {
        // GetResults 会把底层错误对象带过来，所以失败路径也走它。
        AsyncStatus::Canceled => Err(Error::from_hresult(windows::core::HRESULT(E_ABORT))),
        _ => op.GetResults(),
    }
}

/// 同上，用于返回 `IAsyncOperationWithProgress` 的 API（例如 `IInputStream::ReadAsync`）。
pub fn block_on_progress<T, P>(op: IAsyncOperationWithProgress<T, P>) -> Result<T>
where
    T: RuntimeType + 'static,
    P: RuntimeType + 'static,
{
    let state = new_signal();
    let sink = Arc::clone(&state);

    op.SetCompleted(&AsyncOperationWithProgressCompletedHandler::new(
        move |_sender, status| {
            signal(&sink, status);
            Ok(())
        },
    ))?;

    match wait(&state) {
        AsyncStatus::Canceled => Err(Error::from_hresult(windows::core::HRESULT(E_ABORT))),
        _ => op.GetResults(),
    }
}
