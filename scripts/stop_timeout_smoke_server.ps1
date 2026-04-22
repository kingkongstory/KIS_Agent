<#
.SYNOPSIS
Timeout smoke 용으로 띄운 kis_agent 서버를 graceful 하게 내린다.

.DESCRIPTION
`start_timeout_smoke_server.ps1` 가 남긴 `logs/smoke-YYYY-MM-DD-server.pid.json`
을 읽어 wrapper powershell 프로세스의 자식 `kis_agent.exe` 를 찾고, `taskkill /PID`
(없이 `/F`) 로 CTRL_CLOSE_EVENT 를 보낸다. 이는 Rust `tokio::signal::ctrl_c()`
핸들러를 트리거해 `StrategyManager::stop_all()` → 체결 대기 루프 탈출 → cancel
→ `wait_all_stopped(40s)` 순서의 graceful shutdown 경로를 탄다.

`taskkill /F` 는 CLAUDE.md 의 "운영 주의사항" 에서 금지. 반드시 `/F` 없이 호출한다.

.PARAMETER PidFilePath
PID 정보 JSON. 생략 시 `logs/smoke-YYYY-MM-DD-server.pid.json` (KST today).

.PARAMETER ServerBaseUrl
health 폴링 대상. 기본 `http://127.0.0.1:3000`.

.PARAMETER WaitTimeoutSec
종료까지 대기 최대 시간. 기본 45 (Rust graceful 은 40s).

.PARAMETER DryRun
실제 종료 없이 대상 프로세스만 조회해 출력.

.PARAMETER Force
자식 프로세스가 안 잡혀도 wrapper 자체를 `Stop-Process` 로 내린다. 사용 지양.
#>

[CmdletBinding()]
param(
    [string]$PidFilePath,
    [string]$ServerBaseUrl = 'http://127.0.0.1:3000',
    [int]$WaitTimeoutSec = 45,
    [switch]$DryRun,
    [switch]$Force
)

$ErrorActionPreference = 'Stop'

function Resolve-RepoRoot { (Resolve-Path (Join-Path $PSScriptRoot '..')).Path }
function Get-KstToday { ([DateTime]::UtcNow.AddHours(9)).ToString('yyyy-MM-dd') }

function Invoke-Health([string]$ServerBaseUrl) {
    try {
        $null = Invoke-WebRequest -Uri "$ServerBaseUrl/api/v1/monitoring/health" `
            -UseBasicParsing -TimeoutSec 3 -ErrorAction Stop
        return $true
    } catch {
        return $false
    }
}

function Get-ChildProcesses([int]$ParentPid) {
    return @(
        Get-CimInstance Win32_Process -Filter "ParentProcessId=$ParentPid" -ErrorAction SilentlyContinue
    )
}

$repoRoot = Resolve-RepoRoot
Set-Location $repoRoot

if (-not $PidFilePath) {
    $PidFilePath = Join-Path $repoRoot "logs/smoke-$(Get-KstToday)-server.pid.json"
} elseif (-not [System.IO.Path]::IsPathRooted($PidFilePath)) {
    $PidFilePath = Join-Path $repoRoot $PidFilePath
}

if (-not (Test-Path $PidFilePath)) {
    Write-Warning "PID 파일이 없습니다: $PidFilePath"
    Write-Host "서버가 실제로 떠 있는지 확인: $(if (Invoke-Health $ServerBaseUrl) { 'reachable' } else { 'unreachable' })"
    exit 2
}

$pidInfo = Get-Content $PidFilePath -Raw | ConvertFrom-Json
$wrapperPid = [int]$pidInfo.wrapper_pid
Write-Host "=== stop-timeout-smoke-server ==="
Write-Host "pid_file      : $PidFilePath"
Write-Host "wrapper_pid   : $wrapperPid (powershell.exe)"
Write-Host "log_path      : $($pidInfo.log_path)"
Write-Host "server_url    : $($pidInfo.server_base_url)"

# 1. wrapper 자체가 살아있는지
$wrapper = Get-Process -Id $wrapperPid -ErrorAction SilentlyContinue
if (-not $wrapper) {
    Write-Warning "wrapper PID $wrapperPid 이미 종료됨. 고아 kis_agent 를 수동 확인 필요."
    # 추가 안전: 이름으로 kis_agent 탐색
    $orphans = @(Get-Process -Name 'kis_agent' -ErrorAction SilentlyContinue)
    if ($orphans.Count -gt 0) {
        Write-Warning "고아 kis_agent 감지: $($orphans | Select-Object -ExpandProperty Id)"
    }
    if (Invoke-Health $ServerBaseUrl) {
        Write-Warning "그런데 health 는 응답: 다른 인스턴스가 같은 포트에 떠 있을 수 있음."
    }
    exit 3
}

# 2. 자식 kis_agent.exe 찾기
$children = Get-ChildProcesses -ParentPid $wrapperPid
$kisChild = $children | Where-Object { $_.Name -eq 'kis_agent.exe' } | Select-Object -First 1

if ($kisChild) {
    Write-Host "kis_agent_pid : $($kisChild.ProcessId)"
    $targetPid = [int]$kisChild.ProcessId
    $targetLabel = 'kis_agent.exe'
} else {
    Write-Warning "wrapper 아래 kis_agent.exe 자식을 못 찾음. wrapper 자체에 신호 전송 (graceful 여부 불확실)."
    $targetPid = $wrapperPid
    $targetLabel = 'wrapper powershell.exe'
}

if ($DryRun) {
    Write-Host "dry_run       : true"
    Write-Host "would_signal  : $targetLabel pid=$targetPid (taskkill /PID $targetPid — without /F)"
    return
}

# 3. taskkill /PID (without /F) — CTRL_CLOSE_EVENT → Rust tokio ctrl_c 포착
Write-Host "signal        : taskkill /PID $targetPid (graceful, no /F)"
$null = & taskkill /PID $targetPid 2>&1
if ($LASTEXITCODE -ne 0) {
    if ($Force) {
        Write-Warning "taskkill 실패, -Force 로 Stop-Process 시도"
        Stop-Process -Id $targetPid -Force -ErrorAction SilentlyContinue
    } else {
        throw "taskkill 실패 exit=$LASTEXITCODE. -Force 지정 시 Stop-Process 로 강제 종료 가능 (주문 orphan 위험)."
    }
}

# 4. health 폴링
$deadline = [DateTime]::UtcNow.AddSeconds($WaitTimeoutSec)
do {
    Start-Sleep -Seconds 1
    if (-not (Invoke-Health $ServerBaseUrl)) {
        # 프로세스도 함께 사라졌는지 재확인
        $still = Get-Process -Id $targetPid -ErrorAction SilentlyContinue
        if (-not $still) {
            Write-Host "stopped_at    : $(([DateTime]::UtcNow.AddHours(9)).ToString('yyyy-MM-ddTHH:mm:ss'))"
            Write-Host "elapsed_sec   : $([int]($WaitTimeoutSec - ($deadline - [DateTime]::UtcNow).TotalSeconds))"
            return
        }
    }
} while ([DateTime]::UtcNow -lt $deadline)

throw "서버가 $WaitTimeoutSec 초 내 종료되지 않았습니다. HTS/MTS 에서 미체결 주문 수동 확인 필요."
