<#
.SYNOPSIS
장 시작 전 서버를 안전하게 재시작한다 (stop → 5초 대기 → start).

.DESCRIPTION
Windows Task Scheduler 에 매일 평일 08:40 실행으로 등록해 장 시작(09:00) 전
새 session 으로 서버를 재기동한다.

- `scripts/stop_timeout_smoke_server.ps1 -Force` 로 graceful → force fallback
- 5초 대기 (OS 가 포트/핸들 정리)
- `scripts/start_timeout_smoke_server.ps1` 로 재기동
- 기동 실패 시 exit 1 — Task Scheduler 로그에 남음

2026-04-24 추가:
- auto_start 가 서버 시작 시 1회만 실행되므로 매일 장전 재시작으로 러너 재가동
- `wait_until` 수정(동일일 target 경과 시 다음 날 대기)으로 장외 재시작도 안전
  하지만 WS/DB session 회복 + 로그 파일 분리 이점이 있어 장전 재시작이 권장됨
#>

[CmdletBinding()]
param(
    [int]$SleepSecs = 5,
    [int]$StartWaitTimeoutSec = 60
)

$ErrorActionPreference = 'Stop'

function Resolve-RepoRoot { (Resolve-Path (Join-Path $PSScriptRoot '..')).Path }
function Get-KstToday { ([DateTime]::UtcNow.AddHours(9)).ToString('yyyy-MM-dd') }

$repoRoot = Resolve-RepoRoot
Set-Location $repoRoot

$kstToday = Get-KstToday
$kstYesterday = ([DateTime]::UtcNow.AddHours(9).AddDays(-1)).ToString('yyyy-MM-dd')
Write-Host "=== restart-for-market-open ==="
Write-Host "kst_today      : $kstToday"
Write-Host "kst_yesterday  : $kstYesterday"

# 1) 기존 서버 종료 — 오늘자 pid 파일 우선, 없으면 어제자 (장외 재시작 케이스).
$pidToday = Join-Path $repoRoot "logs/smoke-$kstToday-server.pid.json"
$pidYesterday = Join-Path $repoRoot "logs/smoke-$kstYesterday-server.pid.json"

$pidFile = $null
if (Test-Path $pidToday) {
    $pidFile = $pidToday
} elseif (Test-Path $pidYesterday) {
    $pidFile = $pidYesterday
}

if ($pidFile) {
    Write-Host "stopping       : $pidFile"
    try {
        & (Join-Path $repoRoot 'scripts/stop_timeout_smoke_server.ps1') `
            -PidFilePath $pidFile `
            -Force
    } catch {
        Write-Warning "stop script threw: $($_.Exception.Message) — 진행"
    }
} else {
    Write-Warning "기존 pid 파일 없음 (today/yesterday) — 기동 단계만 진행"
}

Start-Sleep -Seconds $SleepSecs

# 2) 새 서버 기동
Write-Host "starting       : 새 인스턴스"
& (Join-Path $repoRoot 'scripts/start_timeout_smoke_server.ps1') `
    -WaitTimeoutSec $StartWaitTimeoutSec

Write-Host "=== restart-for-market-open 완료 ==="
