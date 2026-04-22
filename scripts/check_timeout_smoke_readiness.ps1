<#
.SYNOPSIS
Timeout smoke 장전 readiness 상태를 점검한다.

.DESCRIPTION
아래 항목을 한 번에 확인한다.
- PostgreSQL 실행 여부 (`pg_ctl status`)
- DB TimeZone (`Asia/Seoul`) 일치 여부
- `target/release/kis_agent.exe` 존재 여부
- 권장 서버 로그 경로
- 서버가 이미 떠 있다면 `/api/v1/health`, `/api/v1/monitoring/health` 기준
  `db_connected`, `ws.notification_ready`, `ws_tick_stale`, `ws.terminated`

서버가 아직 안 떠 있으면 health 는 WARN 으로만 표시하고 실패로 보지 않는다.
#>

[CmdletBinding()]
param(
    [string]$PgCtlPath = 'C:\tools\pgsql\bin\pg_ctl.exe',
    [string]$PgDataDir = 'C:\tools\pgsql\data',
    [string]$DbUrl = 'postgres://postgres@localhost:5433/kis_agent',
    [string]$BinaryPath = 'target/release/kis_agent.exe',
    [string]$ServerBaseUrl = 'http://127.0.0.1:3000',
    [string]$ExpectedTimeZone = 'Asia/Seoul',
    [string]$OutFile
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

function Get-DefaultLogPath([string]$RepoRoot) {
    Join-Path $RepoRoot "logs/smoke-$(Get-KstToday)-server.log"
}

function Get-PostgresStatus([string]$PgCtlPath, [string]$PgDataDir) {
    $result = [ordered]@{
        installed = $false
        running   = $false
        status    = 'missing'
        detail    = ''
    }

    if (-not (Test-Path $PgCtlPath)) {
        $result.detail = "pg_ctl 없음: $PgCtlPath"
        return [pscustomobject]$result
    }

    $result.installed = $true
    $output = & $PgCtlPath status -D $PgDataDir 2>&1
    $exitCode = $LASTEXITCODE
    $detail = ($output | Out-String).Trim()
    $result.detail = $detail

    switch ($exitCode) {
        0 {
            $result.running = $true
            $result.status = 'running'
        }
        3 {
            $result.status = 'stopped'
        }
        default {
            $result.status = 'unknown'
        }
    }

    [pscustomobject]$result
}

function Get-DbTimeZone([string]$DbUrl) {
    try {
        $output = & psql $DbUrl -X -A -t -c "SELECT current_setting('TimeZone');" 2>&1
        if ($LASTEXITCODE -ne 0) {
            return [pscustomobject]@{
                ok = $false
                timezone = $null
                error = ($output | Out-String).Trim()
            }
        }
        return [pscustomobject]@{
            ok = $true
            timezone = (($output | Out-String).Trim())
            error = $null
        }
    } catch {
        return [pscustomobject]@{
            ok = $false
            timezone = $null
            error = $_.Exception.Message
        }
    }
}

function Get-JsonEndpoint([string]$Url) {
    try {
        $resp = Invoke-WebRequest -Uri $Url -UseBasicParsing -TimeoutSec 5 -ErrorAction Stop
        return [pscustomobject]@{
            ok = $true
            status_code = [int]$resp.StatusCode
            json = ($resp.Content | ConvertFrom-Json)
            error = $null
        }
    } catch {
        return [pscustomobject]@{
            ok = $false
            status_code = $null
            json = $null
            error = $_.Exception.Message
        }
    }
}

function Write-StatusLine([string]$Label, [string]$State, [string]$Detail) {
    $prefix = switch ($State) {
        'OK' { '[OK]' }
        'WARN' { '[WARN]' }
        default { '[FAIL]' }
    }
    Write-Host ("{0} {1,-24} {2}" -f $prefix, $Label, $Detail)
}

$repoRoot = Resolve-RepoRoot
Set-Location $repoRoot

$resolvedBinaryPath = if ([System.IO.Path]::IsPathRooted($BinaryPath)) {
    $BinaryPath
} else {
    Join-Path $repoRoot $BinaryPath
}

$pg = Get-PostgresStatus -PgCtlPath $PgCtlPath -PgDataDir $PgDataDir
$dbTz = if ($pg.running) { Get-DbTimeZone -DbUrl $DbUrl } else {
    [pscustomobject]@{ ok = $false; timezone = $null; error = 'PostgreSQL not running' }
}
$rootHealth = Get-JsonEndpoint -Url "$ServerBaseUrl/api/v1/health"
$monitoringHealth = Get-JsonEndpoint -Url "$ServerBaseUrl/api/v1/monitoring/health"

$binaryExists = Test-Path $resolvedBinaryPath
$binaryInfo = if ($binaryExists) { Get-Item $resolvedBinaryPath } else { $null }

$healthJson = $monitoringHealth.json
$ws = if ($null -ne $healthJson) { $healthJson.ws } else { $null }
$serverReachable = $rootHealth.ok -and $monitoringHealth.ok
$dbConnected = [bool]($healthJson.db_connected)
$wsNotificationReady = if ($null -ne $ws) { [bool]$ws.notification_ready } else { $false }
$wsTickStale = if ($null -ne $healthJson) { [bool]$healthJson.ws_tick_stale } else { $false }
$wsTerminated = if ($null -ne $ws) { [bool]$ws.terminated } else { $false }
$recommendedLogPath = Get-DefaultLogPath -RepoRoot $repoRoot

$summary = [ordered]@{
    checked_at_kst = Get-KstNow
    postgres = [ordered]@{
        installed = $pg.installed
        running = $pg.running
        status = $pg.status
        detail = $pg.detail
        data_dir = $PgDataDir
    }
    database = [ordered]@{
        db_url = $DbUrl
        timezone_ok = ($dbTz.ok -and $dbTz.timezone -eq $ExpectedTimeZone)
        timezone = $dbTz.timezone
        expected_timezone = $ExpectedTimeZone
        error = $dbTz.error
    }
    binary = [ordered]@{
        exists = $binaryExists
        path = $resolvedBinaryPath
        last_write_time = if ($null -ne $binaryInfo) { $binaryInfo.LastWriteTime.ToString('yyyy-MM-dd HH:mm:ss') } else { $null }
        size = if ($null -ne $binaryInfo) { $binaryInfo.Length } else { $null }
    }
    server = [ordered]@{
        base_url = $ServerBaseUrl
        reachable = $serverReachable
        root_health_ok = $rootHealth.ok
        monitoring_health_ok = $monitoringHealth.ok
        root_health_error = $rootHealth.error
        monitoring_health_error = $monitoringHealth.error
        db_connected = $dbConnected
        ws_notification_ready = $wsNotificationReady
        ws_tick_stale = $wsTickStale
        ws_terminated = $wsTerminated
        ws_retry_count = if ($null -ne $ws) { $ws.retry_count } else { $null }
        recommended_log_path = $recommendedLogPath
    }
    readiness = [ordered]@{
        pre_server = ($pg.running -and $dbTz.ok -and $dbTz.timezone -eq $ExpectedTimeZone -and $binaryExists)
        pre_before_phase = (
            $pg.running -and
            $dbTz.ok -and
            $dbTz.timezone -eq $ExpectedTimeZone -and
            $binaryExists -and
            $serverReachable -and
            $dbConnected -and
            $wsNotificationReady -and
            (-not $wsTickStale) -and
            (-not $wsTerminated)
        )
    }
}

Write-Host "=== timeout-smoke readiness ==="
Write-Host "checked_at_kst : $($summary.checked_at_kst)"
Write-Host "repo_root      : $repoRoot"

Write-StatusLine 'PostgreSQL' `
    ($(if ($pg.running) { 'OK' } elseif ($pg.installed) { 'WARN' } else { 'FAIL' })) `
    $pg.status
Write-StatusLine 'DB TimeZone' `
    ($(if ($dbTz.ok -and $dbTz.timezone -eq $ExpectedTimeZone) { 'OK' } elseif ($pg.running) { 'FAIL' } else { 'WARN' })) `
    ($(if ($dbTz.timezone) { $dbTz.timezone } else { $dbTz.error }))
Write-StatusLine 'Server Binary' `
    ($(if ($binaryExists) { 'OK' } else { 'FAIL' })) `
    $resolvedBinaryPath
Write-StatusLine 'Recommended Log' 'OK' $recommendedLogPath

if ($serverReachable) {
    Write-StatusLine 'HTTP Health' 'OK' "$ServerBaseUrl"
    Write-StatusLine 'DB Connected' ($(if ($dbConnected) { 'OK' } else { 'FAIL' })) "$dbConnected"
    Write-StatusLine 'WS Notification' ($(if ($wsNotificationReady) { 'OK' } else { 'FAIL' })) "$wsNotificationReady"
    Write-StatusLine 'WS Tick Stale' ($(if (-not $wsTickStale) { 'OK' } else { 'FAIL' })) "$wsTickStale"
    Write-StatusLine 'WS Terminated' ($(if (-not $wsTerminated) { 'OK' } else { 'FAIL' })) "$wsTerminated"
} else {
    Write-StatusLine 'HTTP Health' 'WARN' ($monitoringHealth.error ?? $rootHealth.error ?? 'server not reachable')
}

Write-StatusLine 'Ready (pre-server)' ($(if ($summary.readiness.pre_server) { 'OK' } else { 'FAIL' })) "$($summary.readiness.pre_server)"
Write-StatusLine 'Ready (before-phase)' ($(if ($summary.readiness.pre_before_phase) { 'OK' } else { 'WARN' })) "$($summary.readiness.pre_before_phase)"

if ($OutFile) {
    $outPath = if ([System.IO.Path]::IsPathRooted($OutFile)) { $OutFile } else { Join-Path $repoRoot $OutFile }
    $outDir = Split-Path -Parent $outPath
    if ($outDir -and -not (Test-Path $outDir)) {
        New-Item -ItemType Directory -Path $outDir -Force | Out-Null
    }
    ($summary | ConvertTo-Json -Depth 10) | Set-Content -Path $outPath -Encoding UTF8
    Write-Host "saved_json     : $outPath"
}
