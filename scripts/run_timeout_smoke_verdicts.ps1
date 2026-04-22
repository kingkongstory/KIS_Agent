<#
.SYNOPSIS
Timeout smoke evidence 폴더만으로 판정 결과 3개를 재계산한다.

.DESCRIPTION
`manifest.json`, `active-positions-after.json`, `event-log.csv`, `execution-journal.csv`
를 읽어 SQL verdict 와 동일한 규칙으로 archived verdict 를 계산한다.

공식 경로는 archived evidence only 이다. live DB 를 다시 조회하지 않으므로
나중에 다른 머신이나 DB 정리 이후에도 같은 run 폴더만 있으면 재현 가능해야 한다.

필수 archived 파일 중 하나라도 export 실패 payload (`{ "error": ... }` 또는
`error,...`) 이면 `evidence_incomplete:` 오류로 즉시 중단한다.

.PARAMETER RunDir
증거 run 폴더. 예: `docs/monitoring/evidence/2026-04-22-timeout-smoke/run-01`

.PARAMETER DbUrl
호환성용 잔여 파라미터. archived verdict mode 에서는 사용하지 않는다.
#>

[CmdletBinding()]
param(
    [Parameter(Mandatory = $true)][string]$RunDir,
    [string]$DbUrl
)

$ErrorActionPreference = 'Stop'

function Require-File([string]$Path) {
    if (-not (Test-Path $Path)) {
        throw "evidence_incomplete: 필수 파일이 없습니다: $Path"
    }
}

function Read-JsonFile([string]$Path) {
    Require-File $Path
    return Get-Content $Path -Raw | ConvertFrom-Json
}

function Throw-EvidenceIncomplete([string]$Message) {
    throw "evidence_incomplete: $Message"
}

function Assert-JsonExportSucceeded([string]$Path, $Value) {
    if ($null -ne $Value -and $Value.PSObject.Properties.Name -contains 'error') {
        Throw-EvidenceIncomplete "$Path export 실패: $($Value.error)"
    }
}

function Read-ArchivedCsv(
    [string]$Path,
    [string]$Label,
    [string[]]$ExpectedHeaders
) {
    Require-File $Path
    # Get-Content 는 파일 줄이 1개면 string[] 이 아니라 System.String 단일 객체를 반환하고,
    # 그 경우 $rawLines[0] 가 "첫 줄" 대신 "첫 글자" 를 돌려줘 header 검증이 잘못된다.
    # NA 시나리오처럼 header 만 있는 CSV 에서도 동작하도록 항상 배열로 강제한다.
    $rawLines = @(Get-Content -Path $Path)
    if ($rawLines.Count -eq 0) {
        Throw-EvidenceIncomplete "$Label 파일이 비어 있습니다: $Path"
    }

    $header = [string]$rawLines[0]
    $header = $header.TrimStart([char]0xFEFF)
    if ($header -like 'error,*') {
        # header 첫 줄의 에러 본문(", 뒤") 은 항상 포함하고, 추가 줄은 빈 줄을 제외하고 합친다.
        # trailing newline 한 줄만으로 메시지가 비어 보이는 edge case 방지.
        $headerError = ($header -replace '^error,', '')
        $extraLines = @(
            $rawLines |
                Select-Object -Skip 1 |
                Where-Object { -not [string]::IsNullOrWhiteSpace($_) }
        )
        $parts = @()
        if (-not [string]::IsNullOrEmpty($headerError)) { $parts += $headerError }
        if ($extraLines.Count -gt 0) { $parts += $extraLines }
        $errorMessage = $parts -join [Environment]::NewLine
        Throw-EvidenceIncomplete "$Label export 실패: $errorMessage"
    }

    $headers = @(
        $header.Split(',') | ForEach-Object {
            $_.Trim().Trim('"')
        }
    )
    foreach ($expected in $ExpectedHeaders) {
        if ($headers -notcontains $expected) {
            Throw-EvidenceIncomplete "$Label 헤더 누락: $expected ($Path)"
        }
    }

    return @(Import-Csv -Path $Path)
}

function Format-IsoTimestamp($value) {
    if ($null -eq $value) { return '' }
    if ($value -is [DateTime]) { return $value.ToString('yyyy-MM-ddTHH:mm:ss') }
    if ($value -is [DateTimeOffset]) { return $value.ToString('yyyy-MM-ddTHH:mm:sszzz') }
    return [string]$value
}

function Parse-Timestamp([string]$Value, [string]$Label) {
    if ([string]::IsNullOrWhiteSpace($Value)) {
        Throw-EvidenceIncomplete "$Label 시각 값이 비어 있습니다."
    }
    try {
        return [DateTimeOffset]::Parse(
            $Value,
            [System.Globalization.CultureInfo]::InvariantCulture
        )
    } catch {
        Throw-EvidenceIncomplete "$Label 시각 파싱 실패: $Value"
    }
}

function Parse-Metadata([string]$Json, [string]$Label) {
    if ([string]::IsNullOrWhiteSpace($Json)) {
        return [pscustomobject]@{}
    }
    try {
        return $Json | ConvertFrom-Json
    } catch {
        Throw-EvidenceIncomplete "$Label metadata JSON 파싱 실패: $Json"
    }
}

function Get-MetaValue($Metadata, [string]$Key) {
    if ($null -eq $Metadata) { return $null }
    $prop = $Metadata.PSObject.Properties[$Key]
    if ($null -eq $prop) { return $null }
    return $prop.Value
}

function Convert-SectionToLines([object[]]$Rows) {
    if (-not $Rows -or $Rows.Count -eq 0) {
        return @('(none)')
    }
    return @($Rows | ConvertTo-Csv -NoTypeInformation)
}

function Write-VerdictFile([string]$Path, [string[]]$Lines) {
    ($Lines -join [Environment]::NewLine) | Set-Content -Path $Path -Encoding UTF8
    Write-Host "  OK  verdict -> $Path"
}

function Build-TimeoutDedupVerdict(
    [string]$RunDir,
    [string]$StockCode,
    [string]$WindowFrom,
    [string]$WindowTo,
    [object[]]$EventRows
) {
    $relevant = @(
        $EventRows |
            Where-Object {
                $_.stock_code -eq $StockCode -and
                $_.event_type -in @(
                    'actor_timeout_expired',
                    'actor_invalid_transition',
                    'actor_rule_mismatch'
                )
            } |
            Sort-Object event_time_dt
    )

    $timeoutHits = @($relevant | Where-Object { $_.event_type -eq 'actor_timeout_expired' })
    $firstTimeoutAt = if ($timeoutHits.Count -gt 0) {
        ($timeoutHits | Sort-Object event_time_dt | Select-Object -First 1).event_time_dt
    } else {
        $null
    }
    $postTimeoutWarnings = if ($null -ne $firstTimeoutAt) {
        @(
            $relevant |
                Where-Object {
                    $_.event_type -in @('actor_invalid_transition', 'actor_rule_mismatch') -and
                    $_.event_time_dt -ge $firstTimeoutAt
                }
        )
    } else {
        @()
    }

    $invalidCount = @($postTimeoutWarnings | Where-Object { $_.event_type -eq 'actor_invalid_transition' }).Count
    $mismatchCount = @($postTimeoutWarnings | Where-Object { $_.event_type -eq 'actor_rule_mismatch' }).Count
    $verdict = if ($timeoutHits.Count -eq 0) {
        'NA'
    } elseif ($timeoutHits.Count -eq 1 -and $invalidCount -eq 0 -and $mismatchCount -eq 0) {
        'PASS'
    } else {
        'FAIL'
    }

    $timeline = @(
        $relevant | ForEach-Object {
            [pscustomobject]@{
                event_time = $_.event_time
                stock_code = $_.stock_code
                event_type = $_.event_type
                severity   = $_.severity
                message    = $_.message
                phase      = $_.phase
            }
        }
    )
    $summary = [pscustomobject]@{
        timeout_count                 = $timeoutHits.Count
        invalid_count_after_timeout   = $invalidCount
        mismatch_count_after_timeout  = $mismatchCount
        verdict                       = $verdict
    }

    return @(
        "# 10_timeout_expired_dedup (archived)"
        "run_dir: $RunDir"
        "source: event-log.csv"
        "stock_code: $StockCode"
        "window: $WindowFrom ~ $WindowTo"
        ""
        "[timeline]"
        (Convert-SectionToLines $timeline)
        ""
        "[summary]"
        (Convert-SectionToLines @($summary))
    )
}

function Build-ExitManualVerdict(
    [string]$RunDir,
    [string]$StockCode,
    [string]$WindowFrom,
    [string]$WindowTo,
    [object[]]$EventRows,
    $ActivePosition
) {
    $exitTimeoutRows = @(
        $EventRows |
            Where-Object {
                $_.stock_code -eq $StockCode -and
                $_.event_type -eq 'actor_timeout_expired' -and
                $_.phase -eq 'ExitFill'
            } |
            Sort-Object event_time_dt
    )
    $manualRows = @(
        $EventRows |
            Where-Object {
                $_.stock_code -eq $StockCode -and
                $_.event_type -eq 'manual_intervention_required'
            } |
            Sort-Object event_time_dt
    )

    $firstTimeoutAt = if ($exitTimeoutRows.Count -gt 0) {
        ($exitTimeoutRows | Select-Object -First 1).event_time_dt
    } else {
        $null
    }
    $afterManualCount = if ($null -ne $firstTimeoutAt) {
        @($manualRows | Where-Object { $_.event_time_dt -ge $firstTimeoutAt }).Count
    } else {
        0
    }

    $apManual = if ($null -ne $ActivePosition) {
        [bool]$ActivePosition.manual_intervention_required
    } else {
        $false
    }
    $apState = if ($null -ne $ActivePosition -and $null -ne $ActivePosition.execution_state) {
        [string]$ActivePosition.execution_state
    } else {
        ''
    }
    $apLastExitOrder = if ($null -ne $ActivePosition -and $null -ne $ActivePosition.last_exit_order_no) {
        [string]$ActivePosition.last_exit_order_no
    } else {
        ''
    }
    $apLastExitReason = if ($null -ne $ActivePosition -and $null -ne $ActivePosition.last_exit_reason) {
        [string]$ActivePosition.last_exit_reason
    } else {
        ''
    }

    $verdict = if ($exitTimeoutRows.Count -eq 0) {
        'NA'
    } elseif ($afterManualCount -ge 1 -and $apManual -and $apState -eq 'manual_intervention') {
        'PASS'
    } else {
        'FAIL'
    }

    $timeoutTimeline = @(
        $exitTimeoutRows | ForEach-Object {
            [pscustomobject]@{
                event_time = $_.event_time
                event_type = $_.event_type
                phase      = $_.phase
                message    = $_.message
            }
        }
    )
    $manualTimeline = @(
        $manualRows | ForEach-Object {
            [pscustomobject]@{
                event_time = $_.event_time
                event_type = $_.event_type
                message    = $_.message
            }
        }
    )
    $summary = [pscustomobject]@{
        timeout_count          = $exitTimeoutRows.Count
        after_manual_c         = $afterManualCount
        archived_ap_manual     = $apManual
        archived_ap_state      = $apState
        archived_ap_last_exit_order = $apLastExitOrder
        archived_ap_last_exit_reason = $apLastExitReason
        verdict                = $verdict
    }

    return @(
        "# 20_exit_manual_consistency (archived)"
        "run_dir: $RunDir"
        "source: event-log.csv + active-positions-after.json"
        "stock_code: $StockCode"
        "window: $WindowFrom ~ $WindowTo"
        ""
        "[exit_timeout]"
        (Convert-SectionToLines $timeoutTimeline)
        ""
        "[manual_events]"
        (Convert-SectionToLines $manualTimeline)
        ""
        "[summary]"
        (Convert-SectionToLines @($summary))
    )
}

function Build-ArmedDetailVerdict(
    [string]$RunDir,
    [string]$StockCode,
    [string]$WindowFrom,
    [string]$WindowTo,
    [object[]]$EventRows,
    [object[]]$JournalRows
) {
    $awx = @(
        $JournalRows |
            Where-Object {
                $_.stock_code -eq $StockCode -and
                $_.phase -eq 'armed_wait_exit'
            } |
            Sort-Object created_at_dt
    )
    $aweBySignal = @{}
    @(
        $JournalRows |
            Where-Object {
                $_.stock_code -eq $StockCode -and
                $_.phase -eq 'armed_wait_enter'
            } |
            Group-Object signal_id
    ) | ForEach-Object {
        if (-not [string]::IsNullOrWhiteSpace($_.Name)) {
            $aweBySignal[$_.Name] = (
                $_.Group |
                    Sort-Object created_at_dt |
                    Select-Object -First 1
            ).created_at_dt
        }
    }

    $armedEventBySignal = @{}
    @(
        $EventRows |
            Where-Object {
                $_.stock_code -eq $StockCode -and
                $_.event_type -eq 'armed_watch_exit'
            } |
            Sort-Object event_time_dt
    ) | ForEach-Object {
        $signalId = $_.meta_signal_id
        if (-not [string]::IsNullOrWhiteSpace($signalId)) {
            if (-not $armedEventBySignal.ContainsKey($signalId)) {
                $armedEventBySignal[$signalId] = New-Object System.Collections.ArrayList
            }
            [void]$armedEventBySignal[$signalId].Add($_)
        }
    }

    $detailRows = @($awx | Where-Object { $_.detail -eq 'armed_entry_submit_timeout' })
    $enterPrecededCount = 0
    $missingEventCount = 0
    $mismatchCount = 0

    $summaryRows = @(
        $awx | ForEach-Object {
            $journalRow = $_
            $signalId = $journalRow.signal_id
            $enterAt = if ($aweBySignal.ContainsKey($signalId)) { $aweBySignal[$signalId] } else { $null }
            $eventRowsForSignal = if ($armedEventBySignal.ContainsKey($signalId)) {
                @($armedEventBySignal[$signalId])
            } else {
                @()
            }
            $eventExitPresent = $eventRowsForSignal.Count -gt 0
            $eventDetailMatch = @(
                $eventRowsForSignal | Where-Object {
                    [string]$_.meta_detail -eq [string]$journalRow.detail
                }
            ).Count -gt 0

            if ($journalRow.detail -eq 'armed_entry_submit_timeout') {
                if ($null -ne $enterAt -and $enterAt -lt $journalRow.created_at_dt) {
                    $enterPrecededCount += 1
                }
                if (-not $eventExitPresent) {
                    $missingEventCount += 1
                } elseif (-not $eventDetailMatch) {
                    $mismatchCount += 1
                }
            }

            $firstEvent = if ($eventRowsForSignal.Count -gt 0) {
                $eventRowsForSignal[0]
            } else {
                $null
            }

            [pscustomobject]@{
                signal_id          = $signalId
                journal_exit_at    = $journalRow.created_at
                journal_outcome    = $journalRow.outcome
                journal_detail     = $journalRow.detail
                journal_enter_at   = if ($null -ne $enterAt) { $enterAt.ToString('yyyy-MM-ddTHH:mm:sszzz') } else { '' }
                event_exit_present = $eventExitPresent
                event_detail_match = $eventDetailMatch
                event_detail       = if ($null -ne $firstEvent) { $firstEvent.meta_detail } else { '' }
                event_exit_at      = if ($null -ne $firstEvent) { $firstEvent.event_time } else { '' }
            }
        }
    )

    $verdict = if ($awx.Count -eq 0 -and $detailRows.Count -eq 0) {
        'NA'
    } elseif (
        $detailRows.Count -ge 1 -and
        $enterPrecededCount -eq $detailRows.Count -and
        $missingEventCount -eq 0 -and
        $mismatchCount -eq 0
    ) {
        'PASS'
    } else {
        'FAIL'
    }

    $summary = [pscustomobject]@{
        exit_c            = $awx.Count
        detail_c          = $detailRows.Count
        enter_preceded_c  = $enterPrecededCount
        missing_event_c   = $missingEventCount
        mismatch_c        = $mismatchCount
        verdict           = $verdict
    }

    return @(
        "# 30_armed_detail_check (archived)"
        "run_dir: $RunDir"
        "source: execution-journal.csv + event-log.csv"
        "stock_code: $StockCode"
        "window: $WindowFrom ~ $WindowTo"
        ""
        "[summary_rows]"
        (Convert-SectionToLines $summaryRows)
        ""
        "[summary]"
        (Convert-SectionToLines @($summary))
    )
}

if ($DbUrl) {
    Write-Warning "-DbUrl 는 archived verdict mode 에서 사용하지 않습니다. run 폴더의 evidence 만 사용합니다."
}

$resolvedRunDir = (Resolve-Path $RunDir).Path
$manifestPath = Join-Path $resolvedRunDir 'manifest.json'
$activeAfterPath = Join-Path $resolvedRunDir 'active-positions-after.json'
$eventLogPath = Join-Path $resolvedRunDir 'event-log.csv'
$journalPath = Join-Path $resolvedRunDir 'execution-journal.csv'

$manifest = Read-JsonFile $manifestPath
Assert-JsonExportSucceeded $manifestPath $manifest

$activeAfter = Read-JsonFile $activeAfterPath
Assert-JsonExportSucceeded $activeAfterPath $activeAfter

$eventRows = Read-ArchivedCsv `
    -Path $eventLogPath `
    -Label 'event-log.csv' `
    -ExpectedHeaders @('id','event_time','stock_code','category','event_type','severity','message','metadata')
$journalRows = Read-ArchivedCsv `
    -Path $journalPath `
    -Label 'execution-journal.csv' `
    -ExpectedHeaders @(
        'id','created_at','stock_code','execution_id','signal_id','broker_order_id','phase',
        'side','intended_price','submitted_price','filled_price','filled_qty','holdings_before',
        'holdings_after','resolution_source','reason','error_message','metadata','sent_at','ack_at',
        'first_fill_at','final_fill_at'
    )

$stockCode = [string]$manifest.stock_code
if ([string]::IsNullOrWhiteSpace($stockCode)) {
    Throw-EvidenceIncomplete 'manifest.stock_code 가 비어 있습니다.'
}

$runStartedAt = Format-IsoTimestamp $manifest.run_started_at_kst
$runEndedAt = Format-IsoTimestamp $manifest.run_ended_at_kst
if (-not $runStartedAt) { Throw-EvidenceIncomplete 'manifest.run_started_at_kst 가 비어 있습니다.' }
if (-not $runEndedAt)   { Throw-EvidenceIncomplete 'manifest.run_ended_at_kst 가 비어 있습니다.' }

$eventRows = @(
    $eventRows | ForEach-Object {
        $metadata = Parse-Metadata $_.metadata "event-log.csv id=$($_.id)"
        [pscustomobject]@{
            id            = $_.id
            event_time    = $_.event_time
            event_time_dt = Parse-Timestamp $_.event_time "event-log.csv id=$($_.id) event_time"
            stock_code    = $_.stock_code
            category      = $_.category
            event_type    = $_.event_type
            severity      = $_.severity
            message       = $_.message
            metadata      = $metadata
            phase         = [string](Get-MetaValue $metadata 'phase')
            meta_signal_id= [string](Get-MetaValue $metadata 'signal_id')
            meta_detail   = [string](Get-MetaValue $metadata 'detail')
        }
    }
)

$journalRows = @(
    $journalRows | ForEach-Object {
        $metadata = Parse-Metadata $_.metadata "execution-journal.csv id=$($_.id)"
        [pscustomobject]@{
            id            = $_.id
            created_at    = $_.created_at
            created_at_dt = Parse-Timestamp $_.created_at "execution-journal.csv id=$($_.id) created_at"
            stock_code    = $_.stock_code
            signal_id     = $_.signal_id
            phase         = $_.phase
            metadata      = $metadata
            outcome       = [string](Get-MetaValue $metadata 'outcome')
            detail        = [string](Get-MetaValue $metadata 'detail')
        }
    }
)

$activeRows = @()
if ($activeAfter -is [System.Array]) {
    $activeRows = @($activeAfter)
} elseif ($null -ne $activeAfter -and $activeAfter.PSObject.Properties.Name -contains 'stock_code') {
    $activeRows = @($activeAfter)
}
$activePosition = if ($activeRows.Count -gt 0) { $activeRows[0] } else { $null }

Write-Host "=== timeout-smoke verdicts (archived) ==="
Write-Host "  run dir : $resolvedRunDir"
Write-Host "  stock   : $stockCode"
Write-Host "  window  : $runStartedAt ~ $runEndedAt"

$verdict10 = Build-TimeoutDedupVerdict `
    -RunDir $resolvedRunDir `
    -StockCode $stockCode `
    -WindowFrom $runStartedAt `
    -WindowTo $runEndedAt `
    -EventRows $eventRows
$verdict20 = Build-ExitManualVerdict `
    -RunDir $resolvedRunDir `
    -StockCode $stockCode `
    -WindowFrom $runStartedAt `
    -WindowTo $runEndedAt `
    -EventRows $eventRows `
    -ActivePosition $activePosition
$verdict30 = Build-ArmedDetailVerdict `
    -RunDir $resolvedRunDir `
    -StockCode $stockCode `
    -WindowFrom $runStartedAt `
    -WindowTo $runEndedAt `
    -EventRows $eventRows `
    -JournalRows $journalRows

Write-VerdictFile (Join-Path $resolvedRunDir 'verdict-10-timeout-expired.txt') $verdict10
Write-VerdictFile (Join-Path $resolvedRunDir 'verdict-20-exit-manual.txt') $verdict20
Write-VerdictFile (Join-Path $resolvedRunDir 'verdict-30-armed-detail.txt') $verdict30

Write-Host '완료: archived evidence 기준 verdict 파일 3개를 run 폴더에 저장했습니다.'
