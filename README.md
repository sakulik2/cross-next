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
- 进度条（仅 `smtc` 模式，且会话上报时间轴时）；能不能拖动定位取决于 QQ音乐 版本，见下文
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
| `desktop.exe` | 鼠标所在的机器 | 桌面小组件 + 任务栏按钮 + 全局组合键 + 媒体键 |
| `listen.exe` | 鼠标所在的机器 | 常驻，抢媒体键转发（不开 `desktop.exe` 时用） |
| `remote.exe` | 鼠标所在的机器 | 一次性命令 |
| `probe.exe` | 放音乐的机器 | 排查 SMTC 与音频会话 |
| `keyprobe.exe` | 鼠标所在的机器 | 排查按键通路 |
| `update.ps1` | 放音乐的机器 | 原地更新，见「更新」 |

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

**必须在已登录的交互桌面会话里运行。** SMTC 会话按 Windows 登录会话隔离：非交互会话中
`GetSessions()` 返回空，Win11 上 `RequestAsync()` 抛 `0x80070424`。不能做成 Windows 服务，
也不能从 SSH 启动。开机自启用 `shell:startup` 快捷方式，或「仅在用户登录时运行」的计划任务 ——
没有控制台窗口，自启后只在托盘留一个图标。

### 更新

压缩包里带 `update.ps1`，在 exe 所在目录跑它：

```sh
powershell -ExecutionPolicy Bypass -File update.ps1
```

取最新发布，停掉在跑的 `cross-next.exe`、`listen.exe` 与 `desktop.exe`，覆盖 exe，再按原样重启 ——
更新前没在跑的不会被启动。例外：`listen.exe` 与 `desktop.exe` 原来同时在跑时只重启
`desktop.exe`，媒体键改由它接管（`config.json` 里 `media_keys` 为 `false` 时照旧重启 `listen.exe`）。先下载校验再停服务，所以网断了或包坏了只是没更新成，
服务照旧。已经是最新版时直接跳过，不会白停一次。

手动更新等效于：`cross-next.exe --stop`（运行中的 exe 被系统锁着，覆盖前必须先退出），
解压覆盖，重新启动。`--stop` 只关不启，效果和托盘菜单的「退出」一样，区别是能写进脚本。

`config.json` 不在压缩包里，所以覆盖不会动它，token 不变，浏览器和客户端机器上的 `config.json` 那两份
副本继续有效。解压到**新目录**才会生成新 token —— 那种情况把旧的 `config.json` 带过去。

确认在跑的是哪一版：`cross-next.exe --version`（`listen.exe`、`remote.exe`、`desktop.exe` 同样支持，
两个探针打在横幅里）。托盘图标的悬停提示也带版本号 —— 双击启动看不到横幅，那里是唯一的出处。

### config.json

| 字段 | 说明 |
|---|---|
| `target` | 匹配 SMTC AUMID 与进程名的子串，不区分大小写，默认 `qqmusic` |
| `port` | 监听端口，默认 8770 |
| `bind` | 留空则自动挑选内网 IPv4；非空时不走自动挑选 |
| `token` | 首次启动生成，清空则重新生成 |
| `host` | 客户端填：服务端的地址。服务端不读它 |

这份文件也是 `remote.exe`、`listen.exe`、`desktop.exe` 的连接配置，它们读 `host`、`port`、`token`；`desktop.exe` 另外往里存 `topmost`、`media_keys` 和组合键（见[组合键](#组合键)）。各程序只改自己的字段，其余原样保留，所以服务端和客户端在同一台机器上时共用这一份即可。

从旧版升级：第一次启动时，同目录的 `remote.json`、`desktop.json` 里的字段会并进 `config.json`（只补 `config.json` 里没有的），旧文件改名成 `.old` 留作备份；窗口位置不迁移。

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

### 拖动定位要看 QQ音乐 版本

部分 QQ音乐 版本不支持拖动定位。这种版本上进度条只显示进度、不能拖，页面会写明原因。

判断方法是 `/api/state` 的 `canSeek`：实测这个字段在定位上是准的，而
`TryChangePlaybackPositionAsync` 反而会回报"已接受"却不动，所以服务端发出定位后
会读回位置核对，对不上就报错而不是假装成功。

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

开着 `desktop.exe` 就不需要它：`desktop.exe` 默认接管同样的媒体键，启动时会关掉在跑的
`listen.exe`（见[媒体键](#媒体键)）。

```
listen.exe
```

运行期间媒体键只控制远端，本机播放器收不到；退出即恢复。开机自启用 `shell:startup`。

它没有窗口也没有托盘图标，所以：

- **重复启动会自动接管**上一个实例，不必先去关。旧实例收到通知后正常退出并释放热键。
- 要停下来用 `listen.exe --stop`。
- **改 `config.json` 即时生效**，不必重启。换了服务端地址或 token 直接存盘就行。
  文件读坏时保留原配置继续跑。

### 服务端连不上时

空闲 5 分钟后开始探活。连续失败会**放开媒体键**，让它们回到本机播放器，同时后台按
5 到 60 秒退避重连，服务端回来后自动抢回。进程不会自行退出。

所以台式机休眠期间媒体键会控制本机播放器，唤醒后自动恢复 —— 这段时间按键「失灵」
是预期行为。抢回也可能失败（让出期间被别的程序占了），下一轮探活还会再试。
从命令行启动时这些状态变化都会打印。

启动报「媒体键全部注册失败」：若同时提到 `desktop.exe`，是它已经接管了媒体键，不必再开
`listen.exe`；否则是被 cross-next 之外的程序占用 —— 常见的是播放器的全局热键设置、
键盘厂商驱动、其它媒体控制小工具。

### 排查驱动发什么：keyprobe.exe

同时挂三条通路，按侧键后看哪条有输出：

```
[1] 键盘钩子 捕获: 下一首 (VK_MEDIA_NEXT_TRACK)     键盘事件 → listen.exe 可用
[2] Shell 钩子 捕获: 下一首 (APPCOMMAND_...)        WM_APPCOMMAND → 需另写截获
[3] 系统热键 触发: 下一首                           RegisterHotKey 可独占 → listen.exe 可用
```

运行期间同样独占媒体键，退出即恢复。

## 桌面小组件：desktop.exe

不开浏览器的遥控界面，放在鼠标所在的机器上。和网页一样只是界面，走同一套 API，
连接配置读 `config.json`（格式见上文 `remote.exe`）。

```
desktop.exe            启动；已在运行则把窗口叫出来
desktop.exe --stop     关掉正在运行的实例
```

三种操作途径：

| 途径 | 适用 |
|---|---|
| 小组件窗口 | 无边框窗口模式的游戏、副屏 |
| 任务栏缩略图按钮 | 鼠标悬停任务栏图标，预览下方的 ⏮ ⏯ ⏭ |
| 全局组合键 | 独占全屏的游戏（前两种此时看不见） |
| 媒体键 | 键盘媒体键、只能绑预设媒体键的鼠标侧键 |

任务管理器与任务栏右键菜单里显示为「cross-next」。

小组件：

- 默认置顶，位于主屏右下角。按钮以外的地方都能拖，位置不记，下次启动回到右下角。
- **点它不抢焦点**：游戏不会因此失去焦点或松开鼠标。右键菜单例外。
- 右上角有个半透明的 ✕，悬停变实，点一下退出，等同右键菜单的「退出」。连不上服务端时也能点。
- 滚轮调 QQ音乐 音量。
- 右键菜单：置顶开关、最小化到任务栏、在浏览器中打开、组合键状态、接管媒体键开关、退出。

### 媒体键

默认接管 ⏮ ⏯ ⏭ 三个媒体键，作用同 `listen.exe`：只控制远端，本机播放器收不到。
音量键不接管，仍调本机音量。

- 启动时（以及之后每次要接管时）发现 `listen.exe` 在跑，会先把它关掉 —— 两者抢同三个键，
  同时开着只有一个拿得到。
- 服务端连不上时放开，让媒体键回到本机播放器；连上后自动拿回。
- `config.json` 的 `host` 是本机地址（服务端就在这台机器上）时不接管：本机的媒体键本来
  就能直达 QQ音乐，接管反而会和服务端的媒体键模式来回转发。
- 被其它程序占用时，小组件上提示一次，右键菜单里标出是哪个键，每 5 秒重试。
- 不想接管：右键菜单取消勾选「接管媒体键」，或把 `config.json` 的 `media_keys` 改成
  `false`。

### 组合键

| 操作 | 默认 |
|---|---|
| 上一首 / 下一首 | Ctrl+Alt+← / → |
| 播放/暂停 | Ctrl+Alt+空格 |
| 音量 + / - | Ctrl+Alt+↑ / ↓（按住连续调） |
| 静音 | 未设置 |

在 exe 同目录的 `config.json` 里改，这些字段首次运行时自动补全，与其它字段并存：

```json
{
  "topmost": true,
  "media_keys": true,
  "hotkey_prev": "Ctrl+Alt+Left",
  "hotkey_next": "Ctrl+Alt+Right",
  "hotkey_playpause": "Ctrl+Alt+Space",
  "hotkey_volume_up": "Ctrl+Alt+Up",
  "hotkey_volume_down": "Ctrl+Alt+Down",
  "hotkey_mute": ""
}
```

写法是修饰键（`Ctrl` `Alt` `Shift` `Win`）加一个主键，用 `+` 连接，不区分大小写。
主键可以是字母、数字、`F1`-`F24`、`Left` `Right` `Up` `Down` `Space` `Home` `End`
`PageUp` `PageDown` `Insert` `Delete` `Pause`。除 F 键外必须带修饰键。留空表示不绑定。
改完从右键菜单「组合键 → 重新载入」生效，不用重启。

被其它程序占用的组合键注册不上，启动时小组件上会提示，右键菜单里标出是哪个。
部分老 Intel 核显驱动把 Ctrl+Alt+方向键用作旋转屏幕，会占掉默认键位。


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
CSP。单连接数、请求行长度、请求体大小都有上限，三项都在校验 token 之前生效。

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
- `canSeek` — 应用自报是否允许拖动定位。实测这一位在定位上是诚实的（报 false 就真的
  不支持），前端据此禁用滑杆并写明原因。反倒是
  `TryChangePlaybackPositionAsync` 会谎报接受，所以服务端发出定位后还会读回位置核对。

命令类接口返回 `{"accepted": bool}`。`false` 表示 QQ音乐 拒绝了该命令，不是错误。
