param([Parameter(Mandatory)][string]$ProjectDirectory)
$ErrorActionPreference = 'Stop'
$logDirectory = Join-Path ([IO.Path]::GetFullPath($ProjectDirectory)) 'logs'
$rows = @(Get-ChildItem -LiteralPath $logDirectory -Filter 'adaptation-*.jsonl' -File | Sort-Object Name | ForEach-Object {
    $logFile = $_
    foreach ($line in [IO.File]::ReadLines($_.FullName)) {
        if (-not [string]::IsNullOrWhiteSpace($line)) {
            try { $line | ConvertFrom-Json }
            catch { Write-Warning "Skipped incomplete log line in $($logFile.Name)." }
        }
    }
})
if ($rows.Count -eq 0) { throw 'No adaptation timing logs found.' }
$modelRows = @($rows | Where-Object { $_.entry.operation -eq 'model' })
$toolRows = @($rows | Where-Object { $_.entry.operation -eq 'tool' })
function Sum-Field($items, [string]$field) {
    [double](($items | ForEach-Object { $_.entry.$field } | Measure-Object -Sum).Sum)
}
$tools = @($toolRows | Group-Object { $_.entry.tool } | ForEach-Object {
    $group = $_.Group
    [pscustomobject]@{
        tool = $_.Name; calls = $_.Count
        seconds = [Math]::Round((Sum-Field $group 'elapsedMs') / 1000, 2)
        failures = @($group | Where-Object { $_.entry.status -eq 'failed' }).Count
        outputBytes = Sum-Field $group 'outputBytes'
    }
} | Sort-Object calls -Descending)
$repeated = @($toolRows | Where-Object { $_.entry.call.argumentFingerprint } |
    Group-Object { $_.entry.tool + ':' + $_.entry.call.argumentFingerprint } |
    Where-Object Count -gt 1 | Sort-Object Count -Descending | Select-Object -First 20 | ForEach-Object {
        [pscustomobject]@{ tool = $_.Group[0].entry.tool; calls = $_.Count
            selectors = $_.Group[0].entry.call.selectors
            note = 'Identical arguments; compare timing and outcome before treating a repeat as unnecessary.' }
    })
$writes = @($toolRows | Where-Object { $_.entry.tool -eq 'write_file' -and $_.entry.status -eq 'completed' })
$sourceWrites = @($writes | Where-Object { $_.entry.call.selectors.path -match '^src[\\/]' -and $_.entry.result.changed -ne $false })
$fullTests = @($toolRows | Where-Object { $_.entry.tool -eq 'test_adapter' -and $_.entry.call.selectors.discoveryOnly -ne $true -and $_.entry.result.stage -ne 'discovery' })
$lastSourceWrite = $sourceWrites | Select-Object -Last 1
$lastFullTest = $fullTests | Select-Object -Last 1
$afterTest = @($modelRows | Where-Object { $lastFullTest -and $_.at -gt $lastFullTest.at })
$workspaceReads = @($toolRows | Where-Object { $_.entry.tool -eq 'read_file' -and $_.entry.call.selectors.root -eq 'workspace' })
$encodedWorkspaceReads = @($workspaceReads | Where-Object { $_.entry.result.encoding })
$result = [ordered]@{
    project = [IO.Path]::GetFullPath($ProjectDirectory)
    firstEvent = $rows[0].at; lastEvent = $rows[-1].at
    modelCalls = $modelRows.Count
    inputTokens = Sum-Field $modelRows 'inputTokens'; outputTokens = Sum-Field $modelRows 'outputTokens'
    apiSeconds = [Math]::Round((Sum-Field $modelRows 'apiMs') / 1000, 2)
    queueSeconds = [Math]::Round((Sum-Field $modelRows 'queueMs') / 1000, 2)
    toolSeconds = [Math]::Round((Sum-Field $toolRows 'elapsedMs') / 1000, 2)
    rejectedToolBatches = @($rows | Where-Object { $_.entry.operation -eq 'tool-batch-rejected' }).Count
    legacyRows = @($rows | Where-Object { -not $_.schemaVersion -or $_.schemaVersion -lt 2 }).Count
    coverageNote = 'Legacy logs do not record call selectors, argument fingerprints or rejected batches; missing counts are not evidence of no repeats.'
    runs = @($rows | Where-Object { $_.runId } | Group-Object runId | ForEach-Object {
        $group = $_.Group
        $start = $group | Where-Object { $_.entry.operation -eq 'run-start' } | Select-Object -First 1
        $end = $group | Where-Object { $_.entry.operation -eq 'run-end' } | Select-Object -Last 1
        $calls = @($group | Where-Object { $_.entry.operation -eq 'model' })
        [pscustomobject]@{ runId = $_.Name; startedAt = $start.at; status = $end.entry.status
            model = $start.entry.model; calls = $calls.Count
            inputTokens = Sum-Field $calls 'inputTokens'; outputTokens = Sum-Field $calls 'outputTokens'
            maximumTokens = $start.entry.maximumTokens; maximumTurns = $start.entry.maximumTurns
            endedAtTurnLimit = [bool]($end -and $start.entry.maximumTurns -and
                ($end.entry.totalTurns - $start.entry.totalTurns) -ge $start.entry.maximumTurns)
            instructionBytes = $start.entry.instructionBytes; toolDefinitionBytes = $start.entry.toolDefinitionBytes }
    } | Sort-Object startedAt)
    efficiency = [ordered]@{
        sourceWriteCalls = $sourceWrites.Count
        probeWriteCalls = @($writes | Where-Object { $_.entry.call.selectors.path -match '^work[\\/].*\.(cs|py|ps1)$' }).Count
        noteWriteCalls = @($writes | Where-Object { $_.entry.call.selectors.path -match '\.md$' }).Count
        fullVerificationCalls = $fullTests.Count
        parameterFailures = @($toolRows | Where-Object { $_.entry.error -match '参数.*(范围|整数)' }).Count
        implementationReviews = @($rows | Where-Object { $_.entry.operation -eq 'implementation-review' }).Count
        checkpointRequests = @($rows | Where-Object { $_.entry.operation -eq 'checkpoint-request' }).Count
        checkpoints = @($toolRows | Where-Object { $_.entry.tool -eq 'checkpoint' -and $_.entry.status -eq 'completed' }).Count
        protocolReads = @($toolRows | Where-Object { $_.entry.tool -eq 'adapter_protocol' }).Count
        workspaceHexReads = if ($encodedWorkspaceReads.Count -gt 0) { @($encodedWorkspaceReads | Where-Object { $_.entry.result.encoding -eq 'hex' }).Count } else { $null }
        workspaceReadsWithoutEncoding = $workspaceReads.Count - $encodedWorkspaceReads.Count
        lastDirectSourceWrite = if ($lastSourceWrite) { @{ at = $lastSourceWrite.at; turn = $lastSourceWrite.turn; path = $lastSourceWrite.entry.call.selectors.path } } else { $null }
        lastFullVerification = if ($lastFullTest) { @{ at = $lastFullTest.at; turn = $lastFullTest.turn; status = $lastFullTest.entry.status; error = $lastFullTest.entry.error } } else { $null }
        modelCallsSinceLastFullVerification = $afterTest.Count
        tokensSinceLastFullVerification = (Sum-Field $afterTest 'inputTokens') + (Sum-Field $afterTest 'outputTokens')
        note = 'Direct source writes count write_file only; local programs can also edit sources. Metadata is not semantic progress. Older writes may be unchanged; missing run-end does not prove a task is running.'
    }
    tools = $tools; repeatedArguments = $repeated
    recentFailures = @($toolRows | Where-Object { $_.entry.status -eq 'failed' } | Select-Object -Last 12 | ForEach-Object {
        $message = [string]$_.entry.error
        [pscustomobject]@{ at = $_.at; tool = $_.entry.tool; selectors = $_.entry.call.selectors
            error = $message.Substring(0, [Math]::Min(600, $message.Length)) }
    })
}
$result | ConvertTo-Json -Depth 12
