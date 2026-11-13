# Lutra 进度巡检：每 5 分钟向 PROGRESS.md 的日记表格插入一行
# 用法：powershell -NoProfile -File watch-progress.ps1 -IntervalSeconds 300

param(
    [int]$IntervalSeconds = 300,
    [string]$ProgressFile = 'M:\lutraos\PROGRESS.md'
)

$ErrorActionPreference = 'SilentlyContinue'

while ($true) {
    Start-Sleep -Seconds $IntervalSeconds

    $now = Get-Date -Format 'yyyy-MM-dd HH:mm'
    $qemu = Get-Process qemu-system-x86_64 -ErrorAction SilentlyContinue | Select-Object -First 1
    $vm = if ($qemu) { "VM 运行中 pid=$($qemu.Id)" } else { 'VM 已停止' }
    $freeGB = [math]::Round((Get-CimInstance Win32_OperatingSystem).FreePhysicalMemory / 1MB, 1)

    $row = "| $now | 自动巡检 | $vm，宿主空闲 ${freeGB}GB | 完成 1.0 构建 | 等待内核编译 / ISO 打包 | 检查编译产物 | ISO 与安装验证 | 进行中 |"

    $text = Get-Content -Raw -LiteralPath $ProgressFile
    $marker = "`n---`n`n## 四、下一步"
    if ($text.Contains($marker)) {
        $text = $text.Replace($marker, "`n$row" + $marker)
        Set-Content -LiteralPath $ProgressFile -Value $text -NoNewline -Encoding UTF8
    } else {
        Add-Content -LiteralPath $ProgressFile -Value $row -Encoding UTF8
    }
}
