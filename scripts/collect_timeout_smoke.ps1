<#
.SYNOPSIS
Timeout smoke 증거 수집 — docs/monitoring/2026-04-21-timeout-smoke-data-collection-plan.md 절차 4.1/4.2/4.3 을 자동화.

.DESCRIPTION
장중 paper smoke 세션에서 `event_log`, `execution_journal`, `active_positions` DB 스냅샷과
`/api/v1/monitoring/health`, `/api/v1/monitoring/armed-stats`, `/api/v1/strategy/status` API 스냅샷을
`docs/monitoring/evidence/YYYY-MM-DD-timeout-smoke/run-XX/` 에 남긴다.

두 단계로 호출한다.
  -Phase before  : 시나리오 진입 전 (health / *-before / manifest 초기화)
  -Phase after   : 시나리오 종료 직후 (*-after / event-log / execution-journal / manifest 종료 시각)

.PARAMETER Phase
`before` | `after`. `before` 는 OutDir 을 새로 만들고 manifest.json 을 작성하며 시작 시각을 기록한다.
`after` 는 기존 manifest 를 읽어 window 시작 시각을 확보한 뒤 그 이후 DB row 만 export 한다.

.PARAMETER ScenarioId
`armed-timeout` | `entry-fill-timeout` | `exit-fill-timeout`.

.PARAMETER StockCode
타깃 종목. 예: 122630 / 114800.

.PARAMETER SessionDate
`YYYY-MM-DD`. 생략 시 오늘 (KST).

.PARAMETER RunId
기존 run-XX 폴더를 지정해 이어서 기록. 생략 시 `before` 에서 run-01..run-99 중 다음 빈 번호 자동 선택.

.PARAMETER OutRoot
기본값: `docs/monitoring/evidence`. 스크립트 루트 기준 상대 경로.

.PARAMETER ServerBaseUrl
기본값: `http://127.0.0.1:3000`.

.PARAMETER DbUrl
기본값: `$env:DATABASE_URL` 또는 `postgres://postgres@localhost:5433/kis_agent`.

.PARAMETER ServerLogPath
서버 stdout/stderr 로그 파일. 지정 시 `after` 단계에서 run 폴더로 복사.

.PARAMETER WindowMinutes
`event-log.csv` / `execution-journal.csv` 시간창 여유.
- `before`: 생략 시 5분
- `after`: 생략 시 `manifest.json.window_minutes` 재사용, 없으면 5분

.EXAMPLE
scripts/collect_timeout_smoke.ps1 -Phase before -ScenarioId armed-timeout -StockCode 122630

.EXAMPLE
scripts/collect_timeout_smoke.ps1 -Phase after -ServerLogPath logs/server.log
#>

[CmdletBinding()]
param(
    [Parameter(Mandatory = $true)][ValidateSet('before','after')][string]$Phase,
    [ValidateSet('armed-timeout','entry-fill-timeout','exit-fill-timeout')][string]$ScenarioId,
    [string]$StockCode,
    [string]$SessionDate,
    [string]$RunId,
    [string]$OutRoot = 'docs/monitoring/evidence',
    [string]$ServerBaseUrl = 'http://127.0.0.1:3000',
    [string]$DbUrl,
    [string]$ServerLogPath,
    [int]$WindowMinutes = 0,
    [switch]$Force
)

$ErrorActionPreference = 'Stop'
$env:PGCLIENTENCODING = 'UTF8'

# --- 유틸 -----------------------------------------------------------

function Get-KstNow {
    # Windows 가 KST 를 알아두도록 utc+9 로 계산 (시스템 TZ 무관)
    $utc = [DateTime]::UtcNow
    $kst = $utc.AddHours(9)
    return $kst.ToString('yyyy-MM-ddTHH:mm:ss')
}

function Get-KstToday {
    $utc = [DateTime]::UtcNow
    $kst = $utc.AddHours(9)
    return $kst.ToString('yyyy-MM-dd')
}

function Resolve-RepoRoot {
    # 스크립트는 repo/scripts/ 아래라 가정
    return (Resolve-Path (Join-Path $PSScriptRoot '..')).Path
}

function Ensure-Dir($path) {
    if (-not (Test-Path $path)) {
        New-Item -ItemType Directory -Path $path -Force | Out-Null
    }
}

function Invoke-Api($url, $outFile) {
    try {
        $resp = Invoke-WebRequest -Uri $url -UseBasicParsing -TimeoutSec 10 -ErrorAction Stop
        $resp.Content | Set-Content -Path $outFile -Encoding UTF8
        Write-Host "  OK  $url -> $outFile"
    } catch {
        $payload = @{
            error   = $_.Exception.Message
            url     = $url
            at_kst  = (Get-KstNow)
        } | ConvertTo-Json
        $payload | Set-Content -Path $outFile -Encoding UTF8
        Write-Warning "  FAIL $url -> $outFile ($($_.Exception.Message))"
    }
}

function Invoke-Psql-Json($dbUrl, $sql, $outFile) {
    # json_agg 쿼리 결과를 -A -t 모드로 받아 파일에 저장
    $wrapped = "SELECT COALESCE(json_agg(row_to_json(t)), '[]'::json) FROM ($sql) t;"
    $tmp = [System.IO.Path]::GetTempFileName()
    try {
        $wrapped | Set-Content -Path $tmp -Encoding UTF8
        $out = & psql $dbUrl -X -A -t -q -f $tmp 2>&1
        if ($LASTEXITCODE -ne 0) {
            throw "psql exit=$LASTEXITCODE : $out"
        }
        # psql -A -t 는 boolean/숫자를 그대로 내보내므로 json_agg 문자열 그대로 ok
        ($out | Out-String).Trim() | Set-Content -Path $outFile -Encoding UTF8
        Write-Host "  OK  DB -> $outFile"
    } catch {
        $err = @{ error = $_.Exception.Message; at_kst = (Get-KstNow) } | ConvertTo-Json
        $err | Set-Content -Path $outFile -Encoding UTF8
        Write-Warning "  FAIL DB -> $outFile ($($_.Exception.Message))"
    } finally {
        Remove-Item $tmp -ErrorAction SilentlyContinue
    }
}

function Invoke-Psql-Csv($dbUrl, $sql, $outFile) {
    # \copy 는 클라이언트 쪽 경로 사용. forward slash OK.
    $outForward = $outFile -replace '\\','/'
    $copy = "\copy ($sql) TO '$outForward' WITH CSV HEADER"
    try {
        $out = & psql $dbUrl -X -q -c $copy 2>&1
        if ($LASTEXITCODE -ne 0) {
            throw "psql exit=$LASTEXITCODE : $out"
        }
        Write-Host "  OK  DB CSV -> $outFile"
    } catch {
        "error,$($_.Exception.Message.Replace(',',';'))" | Set-Content -Path $outFile -Encoding UTF8
        Write-Warning "  FAIL DB CSV -> $outFile ($($_.Exception.Message))"
    }
}

function Write-RunBanner($phase, $sessionRoot, $runDir, $scenarioId, $stockCode, $dbUrl, $serverBaseUrl) {
    Write-Host "=== timeout-smoke evidence ($phase) ==="
    Write-Host "  session dir : $sessionRoot"
    Write-Host "  run dir     : $runDir"
    Write-Host "  scenario    : $scenarioId"
    Write-Host "  stock_code  : $stockCode"
    Write-Host "  db_url      : $dbUrl"
    Write-Host "  server_url  : $serverBaseUrl"
}

# --- 경로/설정 해석 -------------------------------------------------

$repoRoot = Resolve-RepoRoot
Set-Location $repoRoot

if (-not $SessionDate) { $SessionDate = Get-KstToday }

if (-not $DbUrl) {
    if ($env:DATABASE_URL) { $DbUrl = $env:DATABASE_URL }
    else { $DbUrl = 'postgres://postgres@localhost:5433/kis_agent' }
}

$sessionRoot = Join-Path $repoRoot (Join-Path $OutRoot "$SessionDate-timeout-smoke")
Ensure-Dir $sessionRoot

# run 폴더 결정
if (-not $RunId) {
    if ($Phase -eq 'before') {
        # 다음 빈 번호 자동
        $existing = Get-ChildItem -Path $sessionRoot -Directory -ErrorAction SilentlyContinue `
            | Where-Object { $_.Name -match '^run-(\d+)$' } `
            | ForEach-Object { [int]($_.Name -replace 'run-','') }
        $next = 1
        if ($existing) { $next = ($existing | Measure-Object -Maximum).Maximum + 1 }
        $RunId = "run-$('{0:00}' -f $next)"
    } else {
        # after 단계에서 RunId 생략이면 마지막 폴더 사용
        $latest = Get-ChildItem -Path $sessionRoot -Directory -ErrorAction SilentlyContinue `
            | Where-Object { $_.Name -match '^run-\d+$' } `
            | Sort-Object Name -Descending `
            | Select-Object -First 1
        if (-not $latest) {
            throw "after 단계인데 기존 run 폴더가 없습니다. -RunId 를 명시하거나 먼저 -Phase before 를 실행하세요. (session=$sessionRoot)"
        }
        $RunId = $latest.Name
    }
}

$runDir = Join-Path $sessionRoot $RunId
Ensure-Dir $runDir
$manifestPath = Join-Path $runDir 'manifest.json'
$notesPath    = Join-Path $runDir 'notes.md'

# --- manifest 읽기/작성 --------------------------------------------

if ($Phase -eq 'before') {
    if ($WindowMinutes -le 0) { $WindowMinutes = 5 }
    if ((Test-Path $manifestPath) -and -not $Force) {
        throw "$manifestPath 이미 존재합니다. 이어서 기록하려면 -RunId $RunId -Force 또는 새 RunId 지정하세요."
    }
    if (-not $ScenarioId) { throw 'before 단계는 -ScenarioId 가 필수입니다.' }
    if (-not $StockCode)  { throw 'before 단계는 -StockCode 가 필수입니다.' }

    Write-RunBanner $Phase $sessionRoot $runDir $ScenarioId $StockCode $DbUrl $ServerBaseUrl

    # git commit
    $gitCommit = $null
    try { $gitCommit = (& git rev-parse HEAD 2>$null).Trim() } catch {}

    # environment 추측 (.env)
    $environment = 'paper'
    $envFile = Join-Path $repoRoot '.env'
    if (Test-Path $envFile) {
        $envLine = Get-Content $envFile | Where-Object { $_ -match '^KIS_ENVIRONMENT=' } | Select-Object -First 1
        if ($envLine) { $environment = ($envLine -split '=',2)[1].Trim() }
    }

    $manifest = [ordered]@{
        session_date_kst    = $SessionDate
        scenario_id         = $ScenarioId
        environment         = $environment
        stock_code          = $StockCode
        run_id              = $RunId
        run_started_at_kst  = (Get-KstNow)
        run_ended_at_kst    = $null
        window_minutes      = $WindowMinutes
        server_base_url     = $ServerBaseUrl
        db_url              = $DbUrl
        server_log_file     = $ServerLogPath
        git_commit          = $gitCommit
        collected_files     = [ordered]@{
            health                  = 'health.json'
            strategy_status_before  = 'strategy-status-before.json'
            strategy_status_after   = 'strategy-status-after.json'
            armed_stats_before      = 'armed-stats-before.json'
            armed_stats_after       = 'armed-stats-after.json'
            active_positions_before = 'active-positions-before.json'
            active_positions_after  = 'active-positions-after.json'
            event_log               = 'event-log.csv'
            execution_journal       = 'execution-journal.csv'
            server_log              = 'server.log'
        }
        notes_file          = 'notes.md'
    }

    ($manifest | ConvertTo-Json -Depth 10) | Set-Content -Path $manifestPath -Encoding UTF8

    # notes 템플릿 복사
    $notesTpl = Join-Path $repoRoot 'docs/monitoring/evidence/templates/notes.md'
    if (-not (Test-Path $notesPath) -and (Test-Path $notesTpl)) {
        Copy-Item $notesTpl $notesPath
    }

    # before 스냅샷
    Write-Host "-- API before --"
    Invoke-Api "$ServerBaseUrl/api/v1/monitoring/health"       (Join-Path $runDir 'health.json')
    Invoke-Api "$ServerBaseUrl/api/v1/strategy/status"         (Join-Path $runDir 'strategy-status-before.json')
    Invoke-Api "$ServerBaseUrl/api/v1/monitoring/armed-stats"  (Join-Path $runDir 'armed-stats-before.json')

    Write-Host "-- DB before --"
    $codeLit = $StockCode -replace "'","''"
    $apSql = "SELECT * FROM active_positions WHERE stock_code = '$codeLit'"
    Invoke-Psql-Json $DbUrl $apSql (Join-Path $runDir 'active-positions-before.json')

    Write-Host "before 완료: $manifestPath"
    return
}

# --- Phase = after -------------------------------------------------

if (-not (Test-Path $manifestPath)) {
    throw "after 단계 기준 manifest 파일이 없습니다: $manifestPath. 먼저 -Phase before 실행 필요."
}
$manifest = Get-Content $manifestPath -Raw | ConvertFrom-Json

if (-not $StockCode) { $StockCode = $manifest.stock_code }
if (-not $ScenarioId) { $ScenarioId = $manifest.scenario_id }
if ($WindowMinutes -le 0) {
    if ($manifest.window_minutes -and [int]$manifest.window_minutes -gt 0) {
        $WindowMinutes = [int]$manifest.window_minutes
    } else {
        $WindowMinutes = 5
    }
}

Write-RunBanner $Phase $sessionRoot $runDir $ScenarioId $StockCode $DbUrl $ServerBaseUrl
$runStart = $manifest.run_started_at_kst
if (-not $runStart) { throw 'manifest.run_started_at_kst 가 비어 있습니다. -Phase before 가 정상 완료되지 않았습니다.' }

$nowKst = Get-KstNow
$windowFrom = ([DateTime]::Parse($runStart)).AddMinutes(-$WindowMinutes).ToString('yyyy-MM-ddTHH:mm:ss')
$windowTo   = ([DateTime]::Parse($nowKst)).AddMinutes(1).ToString('yyyy-MM-ddTHH:mm:ss')

Write-Host "-- window: $windowFrom ~ $windowTo --"

# API after
Write-Host "-- API after --"
Invoke-Api "$ServerBaseUrl/api/v1/strategy/status"         (Join-Path $runDir 'strategy-status-after.json')
Invoke-Api "$ServerBaseUrl/api/v1/monitoring/armed-stats"  (Join-Path $runDir 'armed-stats-after.json')

# DB after
$codeLit = $StockCode -replace "'","''"
$fromLit = $windowFrom
$toLit   = $windowTo

Write-Host "-- DB after --"
$apSql = "SELECT * FROM active_positions WHERE stock_code = '$codeLit'"
Invoke-Psql-Json $DbUrl $apSql (Join-Path $runDir 'active-positions-after.json')

$evSql = "SELECT id, event_time, stock_code, category, event_type, severity, message, metadata FROM event_log WHERE event_time BETWEEN '$fromLit'::timestamptz AND '$toLit'::timestamptz AND (stock_code = '$codeLit' OR stock_code = '' OR stock_code IS NULL) ORDER BY event_time"
Invoke-Psql-Csv $DbUrl $evSql (Join-Path $runDir 'event-log.csv')

$ejSql = "SELECT id, created_at, stock_code, execution_id, signal_id, broker_order_id, phase, side, intended_price, submitted_price, filled_price, filled_qty, holdings_before, holdings_after, resolution_source, reason, error_message, metadata, sent_at, ack_at, first_fill_at, final_fill_at FROM execution_journal WHERE created_at BETWEEN '$fromLit'::timestamptz AND '$toLit'::timestamptz AND stock_code = '$codeLit' ORDER BY created_at"
Invoke-Psql-Csv $DbUrl $ejSql (Join-Path $runDir 'execution-journal.csv')

# server log 복사
$logSource = $ServerLogPath
if (-not $logSource) { $logSource = $manifest.server_log_file }
if ($logSource -and (Test-Path $logSource)) {
    Copy-Item $logSource (Join-Path $runDir 'server.log') -Force
    Write-Host "  OK  서버 로그 복사 -> server.log"
} else {
    Write-Warning "server log 경로 없음 또는 파일 없음: $logSource"
}

# manifest 마감
$manifest.run_ended_at_kst = $nowKst
if ($ServerLogPath) { $manifest.server_log_file = $ServerLogPath }
($manifest | ConvertTo-Json -Depth 10) | Set-Content -Path $manifestPath -Encoding UTF8

Write-Host "after 완료: $manifestPath"
Write-Host "판정은 scripts/timeout_smoke_queries/*.sql 을 참고하세요."
