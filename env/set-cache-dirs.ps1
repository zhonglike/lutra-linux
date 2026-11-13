# Lutra Linux - 构建环境变量重定向
# 目的：把所有构建/下载缓存强制留在 M 盘，杜绝写入 C 盘。
# 用法：在 PowerShell 中执行  . .\env\set-cache-dirs.ps1   （注意前面的点，表示在当前会话生效）

$ErrorActionPreference = 'Stop'

$LutraHome = 'M:\lutraos'
$CacheRoot = Join-Path $LutraHome '_cache'

$subDirs = @('tmp', 'ccache', 'xdg', 'node', 'pnpm', 'pip', 'cargo', 'go', 'gradle', 'qemu', 'downloads')
foreach ($d in $subDirs) {
    New-Item -ItemType Directory -Force -Path (Join-Path $CacheRoot $d) | Out-Null
}

# --- 通用临时目录（默认在 C:\Users\...\AppData\Local\Temp，必须改） ---
$env:TMP  = Join-Path $CacheRoot 'tmp'
$env:TEMP = $env:TMP

# --- C/C++ 编译缓存 ---
$env:CCACHE_DIR = Join-Path $CacheRoot 'ccache'

# --- XDG 规范（大量 Linux 工具会读这个） ---
$env:XDG_CACHE_HOME = Join-Path $CacheRoot 'xdg'

# --- Node 系 ---
$env:npm_config_cache     = Join-Path $CacheRoot 'node'
$env:npm_config_store_dir = Join-Path $CacheRoot 'pnpm'
$env:PNPM_HOME            = Join-Path $CacheRoot 'pnpm'
$env:PNPM_STORE_DIR       = Join-Path $CacheRoot 'pnpm'

# --- Python ---
$env:PIP_CACHE_DIR = Join-Path $CacheRoot 'pip'

# --- Rust / Go / Java ---
$env:CARGO_HOME       = Join-Path $CacheRoot 'cargo'
$env:GOPATH           = Join-Path $CacheRoot 'go'
$env:GOMODCACHE       = Join-Path $CacheRoot 'go\pkg\mod'
$env:GRADLE_USER_HOME = Join-Path $CacheRoot 'gradle'

# --- QEMU 临时盘 ---
$env:QEMU_TMPDIR = Join-Path $CacheRoot 'qemu'

# --- 工程路径 ---
$env:LUTRA_HOME = $LutraHome

Write-Host ''
Write-Host '  Lutra 构建环境已重定向到 M 盘' -ForegroundColor Cyan
Write-Host '  ---------------------------------------------'
Write-Host ("  TMP/TEMP      -> {0}" -f $env:TMP)
Write-Host ("  CCACHE_DIR    -> {0}" -f $env:CCACHE_DIR)
Write-Host ("  XDG_CACHE     -> {0}" -f $env:XDG_CACHE_HOME)
Write-Host ("  npm cache     -> {0}" -f $env:npm_config_cache)
Write-Host ("  PIP cache     -> {0}" -f $env:PIP_CACHE_DIR)
Write-Host ("  CARGO_HOME    -> {0}" -f $env:CARGO_HOME)
Write-Host '  ---------------------------------------------'
Write-Host '  注意：本脚本只对当前 PowerShell 会话生效。' -ForegroundColor DarkGray
Write-Host '  不要在本机安装 WSL / Docker Desktop，二者会强制写入 C 盘。' -ForegroundColor Yellow
