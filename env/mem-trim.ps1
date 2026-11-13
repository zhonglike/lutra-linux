# Lutra 内存回收工具
# 原理与 PCL2 的「内存优化」一致：调用 psapi!EmptyWorkingSet 裁剪进程工作集。
# 注意：被裁出的页会进入 standby 列表或页面文件（pagefile.sys），本机 C 盘仅剩 1.1 GB，
#       因此对闲置程序「结束进程」比「裁剪工作集」更实在。
#
# 用法：
#   .\mem-trim.ps1              # 只测量，不执行
#   .\mem-trim.ps1 -Apply       # 实际裁剪（默认不动浏览器）

param(
    [switch]$Apply,
    [int]$MinMB = 50,
    [switch]$IncludeBrowser
)

Add-Type -TypeDefinition @'
using System;
using System.Runtime.InteropServices;
public class LutraMemTrim {
    [DllImport("psapi.dll", SetLastError = true)]
    public static extern bool EmptyWorkingSet(IntPtr hProcess);
    [DllImport("kernel32.dll", SetLastError = true)]
    public static extern IntPtr OpenProcess(uint dwDesiredAccess, bool bInheritHandle, int dwProcessId);
    [DllImport("kernel32.dll", SetLastError = true)]
    public static extern bool CloseHandle(IntPtr hObject);
    public static bool Trim(int pid) {
        IntPtr h = OpenProcess(0x0100 | 0x0400, false, pid);
        if (h == IntPtr.Zero) return false;
        bool ok = EmptyWorkingSet(h);
        CloseHandle(h);
        return ok;
    }
}
'@

$protect = @(
    'System', 'Idle', 'Registry', 'Memory Compression', 'csrss', 'wininit',
    'winlogon', 'services', 'lsass', 'smss', 'dwm', 'fontdrvhost', 'svchost',
    'explorer', 'Trae CN', 'qemu-system-x86_64', 'python', 'python3',
    'powershell', 'pwsh', 'conhost', 'WUDFHost', 'spoolsv'
)
if (-not $IncludeBrowser) {
    $protect += @('chrome', 'msedge', 'firefox', 'brave')
}

$before = (Get-CimInstance Win32_OperatingSystem).FreePhysicalMemory / 1MB

$targets = Get-Process -ErrorAction SilentlyContinue | Where-Object {
    $protect -notcontains $_.ProcessName -and
    $_.WorkingSet64 -gt ($MinMB * 1MB)
} | Sort-Object WorkingSet64 -Descending

Write-Output ('可用内存(前): {0:N2} GB' -f $before)
Write-Output ''
if ($targets) {
    $targets | Select-Object ProcessName, Id,
        @{ n = '工作集MB'; e = { [math]::Round($_.WorkingSet64 / 1MB) } } |
        Format-Table -AutoSize
    $totalMB = [math]::Round((($targets | Measure-Object WorkingSet64 -Sum).Sum) / 1MB)
    Write-Output ("候选进程 {0} 个，工作集合计 {1} MB（实际可回收通常为 30%~60%）" -f $targets.Count, $totalMB)
} else {
    Write-Output '没有满足条件的候选进程。'
}

if ($Apply) {
    Write-Output ''
    $ok = 0
    foreach ($p in $targets) {
        if ([LutraMemTrim]::Trim($p.Id)) { $ok++ }
    }
    Start-Sleep -Seconds 2
    $after = (Get-CimInstance Win32_OperatingSystem).FreePhysicalMemory / 1MB
    Write-Output ('已裁剪 {0}/{1} 个进程' -f $ok, $targets.Count)
    Write-Output ('可用内存(后): {0:N2} GB，净增 {1:N2} GB' -f $after, ($after - $before))
}
