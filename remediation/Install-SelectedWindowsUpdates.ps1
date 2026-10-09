<#
.SYNOPSIS
    Installs only the Windows updates named by KB number. Reference remediation script for the
    NinjaOne Patch Toolkit's "Apply selected OS patches" action.

.DESCRIPTION
    The toolkit dispatches this script through NinjaOne's POST /device/{id}/script/run with a
    parameter string built by src-tauri/src/actions/parameters.rs::build_parameters:

        kbAllowList=5040434,5041580 rebootBehavior=Never dryRun=true

    NinjaOne splits that string on spaces and hands the pieces to the script as arguments. The
    same keys are also read from environment variables (the way NinjaOne exposes declared script
    variables) for any key the argument line does not carry; the argument line wins.

    The script searches the Windows Update Agent for not-installed updates and installs only those
    whose KBArticleIDs intersect the allow list. It never installs anything that is not listed.

    Windows PowerShell 5.1 compatible. No network fetches beyond what the Windows Update Agent
    itself does against the device's configured update service (Microsoft Update or WSUS).

    Exit codes (see remediation/README.md):
        0  every listed KB is installed (or already was); dry run: every listed KB is actionable
        1  bad input - the parameter string is missing, malformed or empty; nothing was touched
        2  nothing matched - no listed KB is offered to, or installed on, this device
        3  incomplete - at least one listed KB failed, is hidden, or was not offered
        4  installed, but a reboot is required and rebootBehavior=Never suppressed it
        5  environment error - Windows Update Agent unavailable, search failed, installer busy,
           or not running elevated

.NOTES
    Dot-sourcing this file (". .\Install-SelectedWindowsUpdates.ps1") only defines the functions;
    remediation/tests/ relies on that to test the parser without touching Windows Update.
#>

#region toolkit-parameter-parser
# Kept byte-identical in Install-SelectedWindowsUpdates.ps1 and Install-SelectedSoftwarePatches.ps1
# (NinjaOne library scripts are single files); the Pester suite asserts the two copies match.

function New-ToolkitInputError {
    param([Parameter(Mandatory = $true)][string]$Message)
    return New-Object System.ArgumentException -ArgumentList $Message
}

function ConvertFrom-ToolkitArgumentLine {
    <#
        Parses the toolkit's space-separated "key=value" argument line into a hashtable keyed by
        the canonical spelling in AllowedKeys. Keys match case-insensitively; an unknown key, a
        repeated key or a token without "=" is an error rather than something to skip, because a
        typo such as "dryrun=false" or "dry_run=false" must not silently fall back to a default.
        An argument that arrives as an array (PowerShell's argument mode turns "a=1,2" into
        @("a=1", "2")) is re-joined with commas first.
    #>
    param(
        [AllowNull()][object[]]$ArgumentList,
        [Parameter(Mandatory = $true)][string[]]$AllowedKeys
    )
    $tokens = New-Object System.Collections.Generic.List[string]
    foreach ($arg in @($ArgumentList)) {
        if ($null -eq $arg) { continue }
        if ($arg -is [System.Array]) {
            $text = (@($arg) | ForEach-Object { [string]$_ }) -join ','
        } else {
            $text = [string]$arg
        }
        foreach ($piece in ($text -split '\s+')) {
            if ($piece.Length -gt 0) { $tokens.Add($piece) }
        }
    }
    $parsed = @{}
    foreach ($token in $tokens) {
        $eq = $token.IndexOf('=')
        if ($eq -lt 1) {
            throw (New-ToolkitInputError "Unrecognized argument '$token': expected key=value.")
        }
        $name = $token.Substring(0, $eq)
        $value = $token.Substring($eq + 1)
        $canonical = $null
        foreach ($key in $AllowedKeys) {
            if ($key -ieq $name) { $canonical = $key; break }
        }
        if ($null -eq $canonical) {
            throw (New-ToolkitInputError "Unknown parameter '$name'. Expected: $($AllowedKeys -join ', ').")
        }
        if ($parsed.ContainsKey($canonical)) {
            throw (New-ToolkitInputError "Parameter '$canonical' was given more than once.")
        }
        $parsed[$canonical] = $value
    }
    return $parsed
}

function Merge-ToolkitEnvironment {
    <#
        Fills every key the argument line did not carry from Environment (NinjaOne injects a
        declared script variable as an environment variable of the same name). The argument line
        always wins: it is what the toolkit composed for this device. An empty environment value
        counts as absent, because NinjaOne sets a declared-but-blank variable to "".
    #>
    param(
        [Parameter(Mandatory = $true)][hashtable]$Parsed,
        [AllowNull()][hashtable]$Environment,
        [Parameter(Mandatory = $true)][string[]]$AllowedKeys
    )
    if ($null -eq $Environment) { return $Parsed }
    foreach ($key in $AllowedKeys) {
        if ($Parsed.ContainsKey($key)) { continue }
        foreach ($envKey in $Environment.Keys) {
            if ($envKey -ieq $key) {
                $value = [string]$Environment[$envKey]
                if (-not [string]::IsNullOrWhiteSpace($value)) { $Parsed[$key] = $value.Trim() }
                break
            }
        }
    }
    return $Parsed
}

function Get-ToolkitEnvironment {
    param([Parameter(Mandatory = $true)][string[]]$AllowedKeys)
    $found = @{}
    foreach ($key in $AllowedKeys) {
        $value = [Environment]::GetEnvironmentVariable($key)
        if ($null -ne $value) { $found[$key] = $value }
    }
    return $found
}

function ConvertTo-ToolkitBoolean {
    # The toolkit sends Rust's bool spelling ("true"/"false"); a NinjaOne checkbox variable
    # arrives the same way. 1/0 are accepted for hand-run convenience; anything else is an error.
    param([Parameter(Mandatory = $true)][string]$Name, [AllowNull()][string]$Value)
    switch -Regex (([string]$Value).Trim()) {
        '^(?i:true|1)$' { return $true }
        '^(?i:false|0)$' { return $false }
    }
    throw (New-ToolkitInputError "Parameter '$Name' must be true or false, got '$Value'.")
}

function ConvertTo-ToolkitRebootBehavior {
    # The toolkit's vocabulary is RebootChoice::script_value in src-tauri/src/actions/kind.rs.
    param([AllowNull()][string]$Value)
    switch -Regex (([string]$Value).Trim()) {
        '^(?i:never)$' { return 'Never' }
        '^(?i:auto)$' { return 'Auto' }
    }
    throw (New-ToolkitInputError "Parameter 'rebootBehavior' must be Never or Auto, got '$Value'.")
}

function Get-ToolkitExitCode {
    <#
        Folds per-target outcomes into the documented exit code. States:
          Installed, AlreadyInstalled, WouldInstall  - the target is (or would be) in place
          NotOffered                                 - the device does not know the target
          Hidden, Failed                             - the target matched but is not installed
          NotImplemented                             - no install mechanism (software script)
    #>
    param(
        [Parameter(Mandatory = $true)][string[]]$States,
        [bool]$RebootPending,
        [bool]$RebootScheduled,
        [bool]$DryRun
    )
    if (@($States | Where-Object { $_ -eq 'NotImplemented' }).Count -gt 0) { return 10 }
    if (@($States | Where-Object { $_ -ne 'NotOffered' }).Count -eq 0) { return 2 }
    $good = @('Installed', 'AlreadyInstalled', 'WouldInstall')
    if (@($States | Where-Object { $good -notcontains $_ }).Count -gt 0) { return 3 }
    if (-not $DryRun -and $RebootPending -and -not $RebootScheduled) { return 4 }
    return 0
}
#endregion toolkit-parameter-parser

function Get-OsParameterKeys { return @('kbAllowList', 'rebootBehavior', 'dryRun') }

function ConvertTo-KbNumberList {
    <#
        "5040434,5041580" -> @("5040434", "5041580"). Every entry must be ASCII digits after an
        optional, case-insensitive "KB" prefix (the same rule as actions::kb_number in the
        toolkit). An empty list, an empty entry or any other character is an error: a script that
        installs nothing and exits 0 reads as success in the toolkit's Jobs tab.
    #>
    param([AllowNull()][string]$Value)
    if ([string]::IsNullOrWhiteSpace($Value)) {
        throw (New-ToolkitInputError 'kbAllowList is empty: refusing to run without an explicit list of KB numbers.')
    }
    $list = New-Object System.Collections.Generic.List[string]
    foreach ($entry in $Value.Split(',')) {
        $trimmed = $entry.Trim()
        # [0-9], not \d: .NET's \d also matches non-ASCII digits.
        $match = [regex]::Match($trimmed, '^(?:[Kk][Bb])?([0-9]{1,10})$')
        if (-not $match.Success) {
            throw (New-ToolkitInputError "kbAllowList entry '$trimmed' is not a KB number (digits only, optional KB prefix).")
        }
        $digits = $match.Groups[1].Value
        if (-not $list.Contains($digits)) { $list.Add($digits) }
    }
    return , $list.ToArray()
}

function Resolve-OsRemediationRequest {
    <#
        The whole input contract in one pure function: argument line + environment in, a
        validated request out, or an ArgumentException. Defaults are the safe ones: a missing
        rebootBehavior means Never and a missing dryRun means true (report only).
    #>
    param([AllowNull()][object[]]$ArgumentList, [AllowNull()][hashtable]$Environment)
    $parsed = ConvertFrom-ToolkitArgumentLine -ArgumentList $ArgumentList -AllowedKeys (Get-OsParameterKeys)
    $parsed = Merge-ToolkitEnvironment -Parsed $parsed -Environment $Environment -AllowedKeys (Get-OsParameterKeys)
    if (-not $parsed.ContainsKey('kbAllowList')) {
        throw (New-ToolkitInputError 'kbAllowList was not provided on the argument line or as a script variable.')
    }
    $reboot = 'Never'
    if ($parsed.ContainsKey('rebootBehavior')) { $reboot = ConvertTo-ToolkitRebootBehavior $parsed['rebootBehavior'] }
    $dryRun = $true
    if ($parsed.ContainsKey('dryRun')) { $dryRun = ConvertTo-ToolkitBoolean -Name 'dryRun' -Value $parsed['dryRun'] }
    return [pscustomobject]@{
        KbNumbers      = [string[]](ConvertTo-KbNumberList $parsed['kbAllowList'])
        RebootBehavior = $reboot
        DryRun         = $dryRun
    }
}

function Test-IsElevated {
    $principal = New-Object Security.Principal.WindowsPrincipal([Security.Principal.WindowsIdentity]::GetCurrent())
    return $principal.IsInRole([Security.Principal.WindowsBuiltInRole]::Administrator)
}

function Get-UpdateKbNumbers {
    param($Update)
    $kbs = @()
    foreach ($kb in $Update.KBArticleIDs) { $kbs += [string]$kb }
    return , $kbs
}

function Get-RebootHint {
    param($Update)
    # InstallationBehavior.RebootBehavior: 0 never, 1 always, 2 can request.
    switch ([int]$Update.InstallationBehavior.RebootBehavior) {
        0 { return 'no reboot' }
        1 { return 'reboot required' }
        default { return 'may require a reboot' }
    }
}

function Get-ResultCodeName {
    param([int]$Code)
    switch ($Code) {
        0 { return 'NotStarted' }
        1 { return 'InProgress' }
        2 { return 'Succeeded' }
        3 { return 'SucceededWithErrors' }
        4 { return 'Failed' }
        5 { return 'Aborted' }
        default { return "Unknown($Code)" }
    }
}

function Search-WindowsUpdate {
    param($Session, [Parameter(Mandatory = $true)][string]$Criteria)
    $searcher = $Session.CreateUpdateSearcher()
    $result = $searcher.Search($Criteria)
    # OperationResultCode: 2 succeeded, 3 succeeded with errors. Anything else is not a list we
    # can trust to say "not offered".
    if ($result.ResultCode -ne 2 -and $result.ResultCode -ne 3) {
        throw "Windows Update search '$Criteria' returned $(Get-ResultCodeName $result.ResultCode)."
    }
    return $result.Updates
}

function Invoke-OsRemediation {
    param([AllowNull()][object[]]$ScriptArguments)

    try {
        $request = Resolve-OsRemediationRequest -ArgumentList $ScriptArguments -Environment (Get-ToolkitEnvironment -AllowedKeys (Get-OsParameterKeys))
    } catch [System.ArgumentException] {
        Write-Host "ERROR (bad input): $($_.Exception.Message)"
        Write-Host 'Expected: kbAllowList=<KB,KB,...> rebootBehavior=<Never|Auto> dryRun=<true|false>'
        return 1
    }

    $mode = 'INSTALL'
    if ($request.DryRun) { $mode = 'DRY RUN - nothing will be installed' }
    Write-Host "Mode: $mode"
    Write-Host "Allow list: $(($request.KbNumbers | ForEach-Object { "KB$_" }) -join ', ')"
    Write-Host "rebootBehavior: $($request.RebootBehavior)"

    if (-not $request.DryRun -and -not (Test-IsElevated)) {
        Write-Host 'ERROR (environment): installing updates needs an elevated session. Run as SYSTEM.'
        return 5
    }

    $states = @{}
    try {
        $session = New-Object -ComObject Microsoft.Update.Session
        $session.ClientApplicationID = 'NinjaOne Patch Toolkit reference remediation'
        $preExistingReboot = (New-Object -ComObject Microsoft.Update.SystemInfo).RebootRequired
        if ($preExistingReboot) { Write-Host 'Note: a reboot was already pending before this run.' }

        Write-Host 'Searching Windows Update for not-installed updates...'
        $pending = Search-WindowsUpdate -Session $session -Criteria 'IsInstalled=0'

        # Only updates whose KBArticleIDs intersect the allow list are ever considered.
        $selected = New-Object System.Collections.ArrayList
        foreach ($update in $pending) {
            $kbs = Get-UpdateKbNumbers $update
            $hit = @($kbs | Where-Object { $request.KbNumbers -contains $_ })
            if ($hit.Count -eq 0) { continue }
            foreach ($kb in $hit) {
                if ($update.IsHidden) {
                    $states[$kb] = 'Hidden'
                    Write-Host "SKIPPED   KB$kb : '$($update.Title)' is hidden on this device; unhide it to install."
                } elseif (-not $states.ContainsKey($kb)) {
                    $states[$kb] = 'Pending'
                }
            }
            if (-not $update.IsHidden) { [void]$selected.Add($update) }
        }

        $unmatched = @($request.KbNumbers | Where-Object { -not $states.ContainsKey($_) })
        if ($unmatched.Count -gt 0) {
            $installed = Search-WindowsUpdate -Session $session -Criteria 'IsInstalled=1'
            foreach ($update in $installed) {
                foreach ($kb in (Get-UpdateKbNumbers $update)) {
                    if ($unmatched -contains $kb -and -not $states.ContainsKey($kb)) {
                        $states[$kb] = 'AlreadyInstalled'
                        Write-Host "INSTALLED KB$kb : already installed ('$($update.Title)')."
                    }
                }
            }
            foreach ($kb in $unmatched) {
                if (-not $states.ContainsKey($kb)) {
                    $states[$kb] = 'NotOffered'
                    Write-Host "NOT FOUND KB$kb : not offered to this device by its update service, and not installed."
                }
            }
        }

        $rebootPending = [bool]$preExistingReboot
        if ($request.DryRun) {
            foreach ($update in $selected) {
                Write-Host "WOULD INSTALL KB$((Get-UpdateKbNumbers $update) -join '/KB') : '$($update.Title)' ($(Get-RebootHint $update))."
            }
            foreach ($kb in @($states.Keys)) { if ($states[$kb] -eq 'Pending') { $states[$kb] = 'WouldInstall' } }
            if ($request.RebootBehavior -eq 'Auto') {
                Write-Host 'Would restart the device afterwards if an update requires it (rebootBehavior=Auto).'
            }
        } elseif ($selected.Count -gt 0) {
            $collection = New-Object -ComObject Microsoft.Update.UpdateColl
            foreach ($update in $selected) {
                if (-not $update.EulaAccepted) { $update.AcceptEula() }
                [void]$collection.Add($update)
            }

            Write-Host "Downloading $($collection.Count) update(s)..."
            $downloader = $session.CreateUpdateDownloader()
            $downloader.Updates = $collection
            $downloadResult = $downloader.Download()
            $installable = New-Object -ComObject Microsoft.Update.UpdateColl
            for ($i = 0; $i -lt $collection.Count; $i++) {
                $update = $collection.Item($i)
                $code = $downloadResult.GetUpdateResult($i).ResultCode
                if ($code -eq 2 -or $update.IsDownloaded) {
                    [void]$installable.Add($update)
                } else {
                    foreach ($kb in (Get-UpdateKbNumbers $update)) {
                        if ($states.ContainsKey($kb)) { $states[$kb] = 'Failed' }
                    }
                    Write-Host "FAILED    KB$((Get-UpdateKbNumbers $update) -join '/KB') : download $(Get-ResultCodeName $code) ('$($update.Title)')."
                }
            }

            if ($installable.Count -gt 0) {
                $installer = $session.CreateUpdateInstaller()
                if ($installer.IsBusy) {
                    Write-Host 'ERROR (environment): another Windows Update installation is in progress.'
                    return 5
                }
                $installer.Updates = $installable
                Write-Host "Installing $($installable.Count) update(s)..."
                $installResult = $installer.Install()
                for ($i = 0; $i -lt $installable.Count; $i++) {
                    $update = $installable.Item($i)
                    $result = $installResult.GetUpdateResult($i)
                    $label = "KB$((Get-UpdateKbNumbers $update) -join '/KB')"
                    $state = 'Failed'
                    if ($result.ResultCode -eq 2) { $state = 'Installed' }
                    foreach ($kb in (Get-UpdateKbNumbers $update)) {
                        # One update failing marks its KB failed even if another update for the
                        # same KB succeeded: the KB is not fully in place.
                        if ($states.ContainsKey($kb) -and $states[$kb] -ne 'Failed') { $states[$kb] = $state }
                    }
                    $reboot = ''
                    if ($result.RebootRequired) { $reboot = ', reboot required' }
                    $verb = 'INSTALLED'
                    if ($state -ne 'Installed') { $verb = 'FAILED   ' }
                    $hresult = '0x{0:X8}' -f $result.HResult
                    Write-Host "$verb $label : $(Get-ResultCodeName $result.ResultCode) (HRESULT $hresult$reboot) '$($update.Title)'."
                }
                if ($installResult.RebootRequired) { $rebootPending = $true }
            }
        }
    } catch {
        Write-Host "ERROR (environment): Windows Update Agent call failed: $($_.Exception.Message)"
        return 5
    }

    $rebootScheduled = $false
    if (-not $request.DryRun -and $rebootPending) {
        if ($request.RebootBehavior -eq 'Auto') {
            # A delayed restart lets this script exit and the NinjaOne agent report the result
            # before the machine goes down.
            & shutdown.exe /r /t 60 /d p:2:17 /c 'NinjaOne Patch Toolkit: restarting to finish installing updates.' | Out-Host
            if ($LASTEXITCODE -eq 0) {
                $rebootScheduled = $true
                Write-Host 'REBOOT    : restart scheduled in 60 seconds (rebootBehavior=Auto).'
            } else {
                Write-Host "REBOOT    : a restart is required but shutdown.exe failed (exit $LASTEXITCODE)."
            }
        } else {
            Write-Host 'REBOOT    : a restart is required to finish; not restarting because rebootBehavior=Never.'
        }
    }

    $allStates = @($request.KbNumbers | ForEach-Object { $states[$_] })
    $exitCode = Get-ToolkitExitCode -States $allStates -RebootPending $rebootPending -RebootScheduled $rebootScheduled -DryRun $request.DryRun
    Write-Host "Result: exit $exitCode"
    return $exitCode
}

if ($MyInvocation.InvocationName -ne '.') {
    # Select the last object so a stray pipeline write can never turn the exit code into an array.
    $code = Invoke-OsRemediation -ScriptArguments $args | Select-Object -Last 1
    exit ([int]$code)
}
