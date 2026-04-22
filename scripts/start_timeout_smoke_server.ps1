<#
.SYNOPSIS
Timeout smoke 용 서버 기동 명령과 로그 경로를 표준화한다.

.DESCRIPTION
기본 동작:
- `target/release/kis_agent.exe server` 를 백그라운드로 실행
- 로그를 `logs/smoke-YYYY-MM-DD-server.log` 로 고정
- PID/로그 경로를 `logs/smoke-YYYY-MM-DD-server.pid.json` 에 기록
- 필요 시 PostgreSQL 을 먼저 기동
- health endpoint 를 polling 해서 `db_connected`, `ws.notification_ready`,
  `ws_tick_stale=false`, `ws.terminated=false` 까지 확인

`-DryRun` 으로 실제 기동 없이 명령/로그 경로만 확인할 수 있다.
#>

[CmdletBinding()]
param(
    [string]$BinaryPath = 'target/release/kis_agent.exe',
    [string]$ServerBaseUrl = 'http://127.0.0.1:3000',
    [string]$LogPath,
    [string]$PidFilePath,
    [int]$WaitTimeoutSec = 45,
    [switch]$EnsurePostgres,
    [string]$PgCtlPath = 'C:\tools\pgsql\bin\pg_ctl.exe',
    [string]$PgDataDir = 'C:\tools\pgsql\data',
    [switch]$SkipWsReady,
    [switch]$DryRun
)

$ErrorActionPreference = 'Stop'

function Resolve-RepoRoot {
    (Resolve-Path (Join-Path $PSScriptRoot '..')).Path
}

function Get-KstNow {
    ([DateTime]::UtcNow.AddHours(9)).ToString('yyyy-MM-ddTHH:mm:ss')
}

function Get-KstToday {
    ([DateTime]::UtcNow.AddHours(9)).ToString('yyyy-MM-dd')
}

function Ensure-Dir([string]$Path) {
    if (-not (Test-Path $Path)) {
        New-Item -ItemType Directory -Path $Path -Force | Out-Null
    }
}

function Invoke-Health([string]$ServerBaseUrl) {
    try {
        $resp = Invoke-WebRequest -Uri "$ServerBaseUrl/api/v1/monitoring/health" -UseBasicParsing -TimeoutSec 5 -ErrorAction Stop
        return [pscustomobject]@{
            ok = $true
            json = ($resp.Content | ConvertFrom-Json)
            error = $null
        }
    } catch {
        return [pscustomobject]@{
            ok = $false
            json = $null
            error = $_.Exception.Message
        }
    }
}

function Ensure-PostgresRunning([string]$PgCtlPath, [string]$PgDataDir) {
    if (-not (Test-Path $PgCtlPath)) {
        throw "pg_ctl 없음: $PgCtlPath"
    }

    & $PgCtlPath status -D $PgDataDir *> $null
    if ($LASTEXITCODE -eq 0) {
        return 'already_running'
    }

    $output = & $PgCtlPath start -D $PgDataDir -l (Join-Path (Split-Path $PgDataDir -Parent) 'log.txt') 2>&1
    if ($LASTEXITCODE -ne 0) {
        throw "PostgreSQL 기동 실패: $($output | Out-String)"
    }
    return 'started'
}

$repoRoot = Resolve-RepoRoot
Set-Location $repoRoot

$resolvedBinaryPath = if ([System.IO.Path]::IsPathRooted($BinaryPath)) {
    $BinaryPath
} else {
    Join-Path $repoRoot $BinaryPath
}
if (-not (Test-Path $resolvedBinaryPath)) {
    throw "서버 바이너리가 없습니다: $resolvedBinaryPath"
}

if (-not $LogPath) {
    $LogPath = Join-Path $repoRoot "logs/smoke-$(Get-KstToday)-server.log"
} elseif (-not [System.IO.Path]::IsPathRooted($LogPath)) {
    $LogPath = Join-Path $repoRoot $LogPath
}

if (-not $PidFilePath) {
    $PidFilePath = Join-Path $repoRoot "logs/smoke-$(Get-KstToday)-server.pid.json"
} elseif (-not [System.IO.Path]::IsPathRooted($PidFilePath)) {
    $PidFilePath = Join-Path $repoRoot $PidFilePath
}

Ensure-Dir (Split-Path -Parent $LogPath)
Ensure-Dir (Split-Path -Parent $PidFilePath)

$existingHealth = Invoke-Health -ServerBaseUrl $ServerBaseUrl
if ($existingHealth.ok) {
    throw "이미 서버가 응답 중입니다: $ServerBaseUrl"
}

if ($EnsurePostgres) {
    $pgStatus = Ensure-PostgresRunning -PgCtlPath $PgCtlPath -PgDataDir $PgDataDir
    Write-Host "postgres       : $pgStatus"
}

$commandText = "& '$resolvedBinaryPath' server *>> '$LogPath'"

Write-Host "=== start-timeout-smoke-server ==="
Write-Host "repo_root      : $repoRoot"
Write-Host "binary         : $resolvedBinaryPath"
Write-Host "server_url     : $ServerBaseUrl"
Write-Host "log_path       : $LogPath"
Write-Host "pid_file       : $PidFilePath"
Write-Host "command        : $commandText"

if ($DryRun) {
    Write-Host 'dry_run        : true'
    return
}

$wrapper = Start-Process `
    -FilePath 'powershell.exe' `
    -ArgumentList @('-NoProfile', '-Command', $commandText) `
    -WorkingDirectory $repoRoot `
    -PassThru `
    -WindowStyle Hidden

$pidInfo = [ordered]@{
    started_at_kst = Get-KstNow
    wrapper_pid = $wrapper.Id
    binary_path = $resolvedBinaryPath
    log_path = $LogPath
    server_base_url = $ServerBaseUrl
    wait_timeout_sec = $WaitTimeoutSec
}
($pidInfo | ConvertTo-Json -Depth 10) | Set-Content -Path $PidFilePath -Encoding UTF8

$deadline = [DateTime]::UtcNow.AddSeconds($WaitTimeoutSec)
do {
    Start-Sleep -Seconds 1
    $health = Invoke-Health -ServerBaseUrl $ServerBaseUrl
    if (-not $health.ok) {
        continue
    }

    $json = $health.json
    $ws = $json.ws
    $dbConnected = [bool]$json.db_connected
    $wsNotificationReady = if ($null -ne $ws) { [bool]$ws.notification_ready } else { $false }
    $wsTickStale = [bool]$json.ws_tick_stale
    $wsTerminated = if ($null -ne $ws) { [bool]$ws.terminated } else { $false }

    $ready = $dbConnected -and (-not $wsTickStale) -and (-not $wsTerminated)
    if (-not $SkipWsReady) {
        $ready = $ready -and $wsNotificationReady
    }

    if ($ready) {
        Write-Host "health_ready   : true"
        Write-Host "db_connected   : $dbConnected"
        Write-Host "ws_ready       : $wsNotificationReady"
        Write-Host "ws_tick_stale  : $wsTickStale"
        Write-Host "ws_terminated  : $wsTerminated"
        Write-Host "wrapper_pid    : $($wrapper.Id)"
        return
    }
} while ([DateTime]::UtcNow -lt $deadline)

throw "서버 기동 후 readiness 조건을 만족하지 못했습니다. log=$LogPath pid_file=$PidFilePath"
