# Lutra 进度巡检：每 5 分钟向 PROGRESS.md 末尾追加一行运行状态
# 用法：powershell -NoProfile -File watch-progress.ps1 -IntervalSeconds 300

param(
    [int]$IntervalSeconds = 300,
    [string]$ProgressFile = 'M:\lutraos\PROGRESS.md'
)

$ErrorActionPreference = 'SilentlyContinue'

while ($true) {
    Start-Sleep -Seconds $IntervalSeconds

    $now = Get-Date -Format 'yyyy-MM-dd HH:mm:ss'

    $qemu = Get-Process qemu-system-x86_64 -ErrorAction SilentlyContinue | Select-Object -First 1
    $vm = if ($qemu) { "运行中 (pid=$($qemu.Id))" } else { '已停止' }

    $disk = 0
    foreach ($f in 'M:\lutraos\_vm\build.qcow2', 'M:\lutraos\_vm\build.vhdx') {
        if (Test-Path $f) { $disk = [math]::Round((Get-Item $f).Length / 1MB) }
    }

    $logKB = 0
    if (Test-Path 'M:\lutraos\_vm\install.log') {
        $logKB = [math]::Round((Get-Item 'M:\lutraos\_vm\install.log').Length / 1KB)
    }

    $freeGB = [math]::Round((Get-CimInstance Win32_OperatingSystem).FreePhysicalMemory / 1MB, 1)

    $row = "| $now | $vm | $disk MB | $logKB KB | $freeGB GB |"
    Add-Content -Path $ProgressFile -Value $row -Encoding UTF8
}
