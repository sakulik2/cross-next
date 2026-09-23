# cross-next

用浏览器遥控另一台 Windows 机器上的 QQ音乐。服务端是单个 exe，网页内嵌其中，客户端零安装。

```
浏览器 ──HTTP/JSON──▶ cross-next.exe
                      ├─ 媒体线程（MTA 单元）
                      │    ├─ SMTC       → 播放控制
                      │    └─ Core Audio → 进程音量
                      └─ HTTP 线程（内嵌网页 + JSON API）
```

## 功能

- 上一首 / 下一首 / 播放暂停
- 进度条与拖动定位（仅 `smtc` 模式，且会话上报时间轴时）
- 曲名、歌手、专辑、封面；封面主色驱动背景渐变
- 单独调 QQ音乐 的进程音量与静音，不影响系统总音量
- 播放期间阻止系统休眠；停止播放 90 秒后放开，之后系统照常按自己的电源策略睡
- 手机可「添加到主屏幕」

不支持搜索点播、播放队列、歌词 —— SMTC 不提供歌曲 ID、列表和歌词。

## 下载

[**最新版**](https://github.com/sakulik2/cross-next/releases/latest/download/cross-next-x86_64-pc-windows-msvc.zip)
— Windows x64，解压即用，不需要装 Rust。

## 产物

| 文件 | 运行在 | 用途 |
|---|---|---|
| `cross-next.exe` | 放音乐的机器 | 服务端，必需 |
| `listen.exe` | 鼠标所在的机器 | 常驻，抢媒体键转发 |
| `remote.exe` | 鼠标所在的机器 | 一次性命令 |
| `probe.exe` | 放音乐的机器 | 排查 SMTC 与音频会话 |
| `keyprobe.exe` | 鼠标所在的机器 | 排查按键通路 |

只用浏览器遥控时只需 `cross-next.exe`。

## 运行

下载的压缩包里已有编译好的 exe，直接跑 `cross-next.exe` 即可。从源码构建：

```sh
cargo build --release
./target/release/cross-next.exe
```

首次运行在 exe 同目录生成 `config.json`，并给出访问地址：

```
    http://192.168.1.1:8770/?t=<32字节随机token>
```

打开一次该链接，前端会把 token 存入 localStorage，之后访问 `http://192.168.1.1:8770/` 即可。

### 托盘

双击运行不出控制台窗口，界面只有托盘图标。右键菜单：

| 菜单项 | 作用 |
|---|---|
| 显示访问地址 | 弹出完整地址并复制到剪贴板 |
| 在本机打开页面 | 用默认浏览器打开（双击图标同效） |
| 退出 | 停止服务端 |

地址随时能从这里取回，不必去翻 `config.json`。鼠标悬停显示的地址不含 token。

从命令行启动时额外把启动横幅打印到控制台，首次配置适合这么跑。端口被占用
（通常是已经开着一个了）会弹消息框说明，不会静默退出。

页面提示 token 无效时，直接在页面上粘贴 `config.json` 里的 token 即可，不必回控制台找链接。
整行 `"token": "..."` 也认，不用自己剥引号。

升级保留 `config.json` 就不会换 token。解压到新目录会生成新的 —— 把旧的 `config.json`
带过去，或直接覆盖旧目录。

**必须在已登录的交互桌面会话里运行。** SMTC 会话按 Windows 登录会话隔离：非交互会话中
`GetSessions()` 返回空，Win11 上 `RequestAsync()` 抛 `0x80070424`。不能做成 Windows 服务，
也不能从 SSH 启动。开机自启用 `shell:startup` 快捷方式，或「仅在用户登录时运行」的计划任务 ——
没有控制台窗口，自启后只在托盘留一个图标。

### config.json

| 字段 | 说明 |
|---|---|
| `target` | 匹配 SMTC AUMID 与进程名的子串，不区分大小写，默认 `qqmusic` |
| `port` | 监听端口，默认 8770 |
| `bind` | 留空则自动挑选内网 IPv4；非空时不走自动挑选 |
| `token` | 首次启动生成，清空则重新生成 |

## 两种控制通路

启动时自动探测，不需配置：

| 模式 | 条件 | 能力 |
|---|---|---|
| `smtc` | QQ音乐 注册了 SMTC 会话 | 定向控制 + 曲名/歌手/专辑/封面 |
| `mediakey` | 无 SMTC 会话但 QQ音乐 在出声 | 仅全局媒体键，无曲目信息 |

不同 QQ音乐 版本行为不同：部分版本注册 SMTC 并提供完整元数据，部分版本完全不注册、
只响应旧的 `WM_APPCOMMAND` 通路（表现为键盘媒体键可用但 `GetSessions()` 返回空）。

`mediakey` 模式的两个限制，界面上会提示：

- 按键是全局的，由系统决定投给哪个应用；其它播放器可能抢走。
- 无曲名、封面、播放状态。

音量控制在两种模式下均可用（走 Core Audio，与 SMTC 无关）。

若 QQ音乐 以管理员身份运行，UIPI 会拦截普通权限进程的按键注入，此时 cross-next
也需以管理员身份运行。

## 用鼠标侧键切歌

### 驱动支持「启动程序」：remote.exe

侧键绑到「启动程序」，参数填命令名：

```
remote.exe next          下一首
remote.exe prev          上一首
remote.exe playpause     播放/暂停
remote.exe vol +10       音量加 10 个百分点
remote.exe vol 60        音量设为 60%
remote.exe mute          静音开关
```

窗口子系统程序，按下不闪黑框。从命令行运行时附加到父进程控制台并打印错误，
双击运行时弹消息框。单条命令约 55ms。

### 驱动只有预设媒体键：listen.exe

部分驱动只提供固定功能列表（上一首/下一首/音量±…），没有「启动程序」。在鼠标所在的
机器上常驻 `listen.exe`，它用 `RegisterHotKey` 独占媒体键并转发到远端。

```
listen.exe
```

运行期间媒体键只控制远端，本机播放器收不到；退出即恢复。开机自启用 `shell:startup`。

它没有窗口也没有托盘图标，所以：

- **重复启动会自动接管**上一个实例，不必先去关。旧实例收到通知后正常退出并释放热键。
- 要停下来用 `listen.exe --stop`。
- **改 `remote.json` 即时生效**，不必重启。换了服务端地址或 token 直接存盘就行。
  文件读坏时保留原配置继续跑。

### 服务端连不上时

空闲 5 分钟后开始探活。连续失败会**放开媒体键**，让它们回到本机播放器，同时后台按
5 到 60 秒退避重连，服务端回来后自动抢回。进程不会自行退出。

所以台式机休眠期间媒体键会控制本机播放器，唤醒后自动恢复 —— 这段时间按键「失灵」
是预期行为。抢回也可能失败（让出期间被别的程序占了），下一轮探活还会再试。
从命令行启动时这些状态变化都会打印。

启动报「媒体键全部注册失败」表示这些键被 cross-next 之外的程序占用 —— 常见的是播放器的
全局热键设置、键盘厂商驱动、其它媒体控制小工具。

### 排查驱动发什么：keyprobe.exe

同时挂三条通路，按侧键后看哪条有输出：

```
[1] 键盘钩子 捕获: 下一首 (VK_MEDIA_NEXT_TRACK)     键盘事件 → listen.exe 可用
[2] Shell 钩子 捕获: 下一首 (APPCOMMAND_...)        WM_APPCOMMAND → 需另写截获
[3] 系统热键 触发: 下一首                           RegisterHotKey 可独占 → listen.exe 可用
```

运行期间同样独占媒体键，退出即恢复。

## 排查

### 检测不到 SMTC 会话

```sh
cargo run --bin probe
```

先判定是否交互桌面会话，再列出所有 SMTC 会话的 AUMID、曲目、能力位、时间轴，以及所有
音频会话的进程名。跑完等回车，双击运行也可见输出。服务端运行时亦可访问
`/api/sessions?t=<token>`。

一个会话都没有时，用浏览器播放视频再跑一次：浏览器出现则 SMTC 框架正常、是 QQ音乐
未注册（会自动降级到 `mediakey`）；仍为空则是系统层面的问题。

### 其它设备连不上

装有 WSL2 或 Hyper-V 的机器存在虚拟网卡，地址多为 `172.x`，能通过 RFC1918 检查，
但属于宿主机内部网段，局域网其它设备无路由可达。

自动挑选优先选取有默认网关的网卡。若仍选错，用 `ipconfig` 找到与路由器同网段的地址
（通常 `192.168.x.x`）填入 `config.json` 的 `bind`。

## 安全边界

明文 HTTP，token 会出现在内网流量中。它防的是同网段其它设备或程序的误触，不防嗅探。
默认只绑内网 IPv4 而非 `0.0.0.0`，虚拟网卡、VPN、公网网卡上不监听。

**不要将此端口转发到公网。**

`config.json` 与 exe 同目录，按所在目录的权限继承 ACL。放在自己的用户目录下即可；
解压到 `Program Files` 或其它所有用户可读的位置，等于把 token 摊给本机所有账户。

`Host` 与绑定地址不符的请求直接拒绝（挡 DNS rebinding），响应带 `nosniff` 与
CSP。单连接数、请求行长度、请求体大小都有上限，其中前两项在校验 token 之前生效。

## API

除 `/` 外均需 token，走 `Authorization: Bearer <token>` 或 `?t=<token>`。

| 路由 | 说明 |
|---|---|
| `GET /` | 内嵌前端页面 |
| `GET /api/state` | 当前曲目、播放状态、音量、控制模式 |
| `POST /api/cmd` | `{"action":"next"\|"prev"\|"playpause"}`；`{"action":"seek","position":<秒>}` |
| `POST /api/volume` | `{"level":0.0-1.0}` 或 `{"mute":true}` |
| `GET /api/thumbnail` | 封面字节，ETag 为内容哈希；`mediakey` 模式恒 404 |
| `GET /api/sessions` | 所有 SMTC 会话 |

`/api/state` 字段：

- `mode` — `smtc` / `mediakey` / `none`，见上表。
- `volume` — `null` 表示 QQ音乐 当前无音频会话（Core Audio 只列出活跃过的会话），
  前端据此置灰滑杆而非显示 0。
- `matched` — false 表示未匹配到 `target`，当前控制的是系统「当前会话」，见 `aumid`。
- `artTag` — 曲目元数据哈希，换歌才变；前端据此决定是否重取封面。
- `position` / `duration` — 秒。`null` 表示该会话不上报时间轴（直播流、
  或播放尚未真正开始），此时前端隐藏进度条。`position` 已由服务端按
  `LastUpdatedTime` 外推到当前时刻。
- `canSeek` — 应用自报是否允许拖动定位。不可靠（QQ音乐 报 false 却照样接受定位），
  前端不据此禁用滑杆。

命令类接口返回 `{"accepted": bool}`。`false` 表示 QQ音乐 拒绝了该命令，不是错误。
