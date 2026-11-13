# 启动 Lutra 构建/测试虚拟机（仅使用 M 盘）
# 用法：
#   .\env\run-vm.ps1                       # 从已有磁盘引导
#   .\env\run-vm.ps1 -Mode Installer       # 从 Debian DVD 引导
#   .\env\run-vm.ps1 -Mode KernelTest -Kernel ... -Initrd ... -Append "..."
param(
    [ValidateSet('Disk', 'Installer', 'KernelTest', 'Cloud')]
    [string]$Mode = 'Disk',
    [int]$MemMB = 4096,
    [int]$Cpu = 8,
    [int]$SshPort = 2222,
    [string]$Disk = 'M:\lutraos\_vm\build.qcow2',
    [string]$Kernel = '',
    [string]$Initrd = '',
    [string]$Append = ''
)

$ErrorActionPreference = 'Stop'

$qemu = 'M:\lutraos\_toolchains\qemu\app\qemu-system-x86_64.exe'
$vmDir = 'M:\lutraos\_vm'
$tmpDir = 'M:\lutraos\_cache\tmp'
New-Item -ItemType Directory -Force -Path $vmDir, $tmpDir | Out-Null

if ($Mode -ne 'KernelTest' -and -not (Test-Path $Disk)) {
    throw "磁盘不存在: $Disk"
}

$qa = @(
    '-accel', 'whpx',
    '-smp', "$Cpu",
    '-m', "$MemMB",
    '-nic', "user,model=virtio-net-pci,hostfwd=tcp::$SshPort-:22",
    '-display', 'none',
    '-chardev', "file,id=log,path=$vmDir/serial.log",
    '-serial', 'chardev:log'
)

switch ($Mode) {
    'Installer' {
        $iso = 'M:\lutraos\_cache\downloads\debian-13.7.0-amd64-DVD-1.iso'
        if (-not (Test-Path $iso)) { throw "安装镜像不存在: $iso" }
        $qa += @('-cdrom', $iso, '-boot', 'd')
    }
    'Cloud' {
        $seed = 'M:\lutraos\_vm\seed.iso'
        if (-not (Test-Path $seed)) { throw "cloud-init 种子盘不存在: $seed" }
        $qa += @('-drive', "file=$Disk,if=virtio,format=qcow2", '-cdrom', $seed, '-boot', 'c')
    }
    default {
        $qa += @('-drive', "file=$Disk,if=virtio,format=qcow2")
    }
}

if ($Kernel) { $qa += @('-kernel', $Kernel) }
if ($Initrd) { $qa += @('-initrd', $Initrd) }
if ($Append) { $qa += @('-append', $Append) }

$p = Start-Process -FilePath $qemu -ArgumentList $qa -PassThru -WindowStyle Hidden `
    -RedirectStandardError "$tmpDir\qemu.err"

Write-Output "VM pid=$($p.Id) mode=$Mode mem=${MemMB}MB cpu=$Cpu ssh=127.0.0.1:$SshPort"
