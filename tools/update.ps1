# cross-next 原地更新。
#
# 用法：在 exe 所在目录右键「使用 PowerShell 运行」，或
#   powershell -ExecutionPolicy Bypass -File update.ps1
#
# 做的事：取最新发布 → 停掉在跑的实例 → 覆盖 exe → 按原样重启。
#
# 两个刻意的顺序选择：
#
#   1. **先下载校验，再停服务。** 反过来的话，网断了或压缩包坏了就会留下一个
#      「已经停了但新文件没到」的状态 —— 用户什么都没了，还得自己去翻发布页。
#      现在最坏情况是服务照常跑着，只是没更新成。
#   2. **不删旧目录，只覆盖文件。** config.json 就在 exe 边上，删目录会把 token
#      一起带走，浏览器和 listen.exe 那两份副本随之失效（见 README 的安全边界）。
#      压缩包里本来就没有 config.json，逐个覆盖天然保住它。

$ErrorActionPreference = "Stop"

$Repo  = "sakulik2/cross-next"
$Asset = "cross-next-x86_64-pc-windows-msvc.zip"

# SHA-256，走 .NET 而不是 Get-FileHash。
#
# 实测原因：装了 PowerShell 7 的机器上，`PSModulePath` 里 PS7 的模块目录
# （...\windowsapps\microsoft.powershell_7.x...\Modules）可能排在 Windows PowerShell
# 自己的前面。5.1 于是优先找到 PS7 版的 Utility 模块，而那个在 5.1 上加载不了，
# 结果 `Get-FileHash` 直接报 CommandNotFoundException —— 同一台机器上
# `Expand-Archive` 和 `Invoke-WebRequest` 却正常，所以这不是「模块全坏了」，
# 光测一个 cmdlet 在不在推不出别的。.NET 类型不经过模块加载，不受这件事影响。
function Get-Sha256 {
    param([string]$Path)
    $sha = [Security.Cryptography.SHA256]::Create()
    try {
        $stream = [IO.File]::OpenRead((Resolve-Path $Path).Path)
        try {
            ($sha.ComputeHash($stream) | ForEach-Object { $_.ToString('x2') }) -join ''
        } finally { $stream.Dispose() }
    } finally { $sha.Dispose() }
}

# 脚本所在目录就是安装目录 —— 它随压缩包一起解压到 exe 旁边。
$InstallDir = $PSScriptRoot
if (-not $InstallDir) { $InstallDir = (Get-Location).Path }

Write-Host "cross-next 更新"
Write-Host "  安装目录: $InstallDir"
Write-Host ""

# ---- 1. 下载到临时目录 ----

$work = Join-Path $env:TEMP "cross-next-update-$(Get-Random)"
New-Item -ItemType Directory -Path $work | Out-Null

try {
    # TLS 1.2：老 PowerShell 默认还在用 TLS 1.0，GitHub 已经不接了。
    [Net.ServicePointManager]::SecurityProtocol = [Net.SecurityProtocolType]::Tls12

    $zip = Join-Path $work $Asset
    $url = "https://github.com/$Repo/releases/latest/download/$Asset"

    Write-Host "  下载中: $url"
    # 关掉进度条，Invoke-WebRequest 画进度在慢速连接上会显著拖慢下载。
    $prev = $ProgressPreference
    $ProgressPreference = "SilentlyContinue"
    try {
        Invoke-WebRequest -Uri $url -OutFile $zip -UseBasicParsing
    } finally {
        $ProgressPreference = $prev
    }

    # 同样走 .NET 而不是 Expand-Archive：那也是模块 cmdlet，和上面 Get-Sha256 注释里
    # 说的 PS7 遮蔽属于同一类风险。实测本机可用，但这个脚本跑在服务端那台机器上，
    # 少一个模块依赖就少一处只能在对方机器上才暴露的失败。
    Add-Type -AssemblyName System.IO.Compression.FileSystem
    [IO.Compression.ZipFile]::ExtractToDirectory($zip, $work)

    # Compress-Archive 打包的是整个目录，所以解出来多一层同名文件夹。
    $src = Join-Path $work ([IO.Path]::GetFileNameWithoutExtension($Asset))
    if (-not (Test-Path $src)) {
        throw "压缩包结构和预期不符，没找到 $src"
    }

    # 停服务之前先确认新文件真的在，别把用户留在「两头空」的状态。
    $exes = @("cross-next.exe", "probe.exe", "remote.exe", "keyprobe.exe", "listen.exe")
    foreach ($exe in $exes) {
        if (-not (Test-Path (Join-Path $src $exe))) {
            throw "压缩包里缺 $exe，中止更新（当前安装未被改动）"
        }
    }
    Write-Host "  校验通过，5 个 exe 齐全"

    # 已经是最新版就别白停一次服务。
    #
    # 比的是文件哈希，**不是** `cross-next.exe --version` 的输出，有两个实测理由：
    #   1. 窗口子系统程序在 PowerShell 里用 `&` 调用抓不到 stdout（实测得 $null；
    #      bash 能抓到，`Start-Process -RedirectStandardOutput` 到文件也能）。
    #      照那么写这段会变成永远不成立的死代码。
    #   2. 更要紧的是，万一 exe 判定自己没有控制台，`ui::report` 会弹模态消息框，
    #      脚本就永久挂在那里等人点确定。不启动进程就没有这个风险。
    #
    # 哈希也更贴题：要问的本来就是「下载来的和装着的是不是同一个文件」。
    # 附带好处是旧版没有 --version 也照样能比。
    $oldExe = Join-Path $InstallDir "cross-next.exe"
    if (Test-Path $oldExe) {
        $newHash = Get-Sha256 (Join-Path $src "cross-next.exe")
        $oldHash = Get-Sha256 $oldExe
        if ($newHash -eq $oldHash) {
            Write-Host ""
            Write-Host "已是最新版，无需更新。"
            return
        }
    }
    Write-Host ""

    # ---- 2. 停掉在跑的实例 ----

    # 记下原来在跑什么，更新完按原样恢复 —— 用户没要求改变运行状态。
    $ranServer = $null -ne (Get-Process -Name "cross-next" -ErrorAction SilentlyContinue)
    $ranListen = $null -ne (Get-Process -Name "listen" -ErrorAction SilentlyContinue)

    # 用各自的 --stop 而不是 Stop-Process：走正常退出路径才会摘掉托盘图标
    # （否则留一个幽灵）、才会 UnregisterHotKey 放开媒体键。
    foreach ($item in @(@("cross-next", $ranServer), @("listen", $ranListen))) {
        $name = $item[0]
        if (-not $item[1]) { continue }
        $exe = Join-Path $InstallDir "$name.exe"
        if (-not (Test-Path $exe)) { continue }
        Write-Host "  停止 $name.exe"
        & $exe --stop | Out-Null
    }

    # --stop 返回时窗口已消失，但进程对象要再一瞬才释放 exe 的文件锁。
    # 轮询到真的没了，比睡一个固定时长可靠。
    foreach ($name in @("cross-next", "listen")) {
        for ($i = 0; $i -lt 100; $i++) {
            if (-not (Get-Process -Name $name -ErrorAction SilentlyContinue)) { break }
            Start-Sleep -Milliseconds 50
        }
        if (Get-Process -Name $name -ErrorAction SilentlyContinue) {
            throw "$name.exe 没有退出，无法覆盖。请手动关掉它再重试。"
        }
    }

    # ---- 3. 覆盖文件 ----

    # 逐个复制而不是整目录替换，这样 config.json 留在原地。
    Copy-Item -Path (Join-Path $src "*") -Destination $InstallDir -Recurse -Force
    Write-Host "  已覆盖 exe 与 README"
    Write-Host ""

    # ---- 4. 按原样重启 ----

    if ($ranServer) {
        Write-Host "  重启 cross-next.exe"
        # Start-Process 而不是直接调用：它是窗口子系统程序，直接调会让脚本挂着等。
        Start-Process -FilePath (Join-Path $InstallDir "cross-next.exe") -WorkingDirectory $InstallDir
    }
    if ($ranListen) {
        Write-Host "  重启 listen.exe"
        Start-Process -FilePath (Join-Path $InstallDir "listen.exe") -WorkingDirectory $InstallDir
    }

    Write-Host ""
    Write-Host "更新完成。config.json 未改动，token 不变，浏览器不用重新填。"
    if (-not $ranServer) {
        Write-Host "（更新前 cross-next 没在跑，所以没有自动启动它。）"
    }
}
finally {
    Remove-Item -Path $work -Recurse -Force -ErrorAction SilentlyContinue
}
