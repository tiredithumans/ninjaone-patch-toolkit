<#
.SYNOPSIS
    Reference remediation script for the NinjaOne Patch Toolkit's "Apply selected software
    patches" action. Decodes and validates the toolkit's product allow list and hands each product
    to an install hook YOU supply. Out of the box it installs nothing and says so.

.DESCRIPTION
    The toolkit dispatches this script through NinjaOne's POST /device/{id}/script/run with a
    parameter string built by src-tauri/src/actions.rs::build_parameters:

        productAllowListB64=R29vZ2xlIENocm9tZSAxNDEuMC43MzkwLjU1fDctWmlw rebootBehavior=Never dryRun=true

    productAllowListB64 is standard base64 (RFC 4648 alphabet A-Z a-z 0-9 + /, "=" padding) of the
    UTF-8 bytes of the selected patches' NinjaOne titles joined by "|" - here
    "Google Chrome 141.0.7390.55|7-Zip". Titles carry spaces and NinjaOne splits the parameter
    string on spaces, which is why the list travels encoded.

    WHY THIS SCRIPT DOES NOT INSTALL ANYTHING BY DEFAULT
    NinjaOne's third-party patching is performed by the NinjaOne agent itself; there is no local,
    supported command that says "install NinjaOne third-party patch <title>". The WinGet CLI is not
    supported as SYSTEM (the context NinjaOne scripts run in), and mapping a NinjaOne title such as
    "Google Chrome 141.0.7390.55" to a package id is a per-tenant decision. So this script does the
    part that is the same everywhere - strict decoding, validation, per-product reporting, reboot
    handling and exit codes - and delegates the install to Install-SelectedProduct below, which
    returns NotImplemented until you replace it (for example with the Microsoft.WinGet.Client
    module, which does support SYSTEM for machine-wide packages, or with Chocolatey). While it
    returns NotImplemented, every run - including a dry run - exits 10 rather than 0, so the
    toolkit's Jobs tab never shows a success for work that did not happen.

    Windows PowerShell 5.1 compatible. The script makes no network fetches of its own.

    Exit codes (see remediation/README.md):
        0  every listed product is installed (or already up to date); dry run: every one is actionable
        1  bad input - the parameter string is missing, malformed, not base64/UTF-8, or empty
        2  nothing matched - the install hook reported every product as not offered on this device
        3  incomplete - at least one product failed or was not offered while others were handled
        4  installed, but a reboot is required and rebootBehavior=Never suppressed it
        5  environment error - not elevated, or the install hook threw
        10 no install mechanism - Install-SelectedProduct has not been implemented

.NOTES
    Dot-sourcing this file (". .\Install-SelectedSoftwarePatches.ps1") only defines the functions;
    remediation/tests/ relies on that to test the decoder.
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
    # The toolkit's vocabulary is RebootChoice::script_value in src-tauri/src/actions.rs.
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

function Get-SoftwareParameterKeys { return @('productAllowListB64', 'rebootBehavior', 'dryRun') }

function ConvertFrom-ProductAllowList {
    <#
        Decodes productAllowListB64 exactly as the toolkit encodes it: standard base64 with
        padding, UTF-8 inside, titles joined by "|". Anything else is an error, including an empty
        list and an empty title - a script that installs nothing and exits 0 reads as success.
        Duplicate titles are dropped; order is preserved. Titles are passed on verbatim (no
        trimming), because they are NinjaOne's own strings.
    #>
    param([AllowNull()][string]$Value)
    if ([string]::IsNullOrWhiteSpace($Value)) {
        throw (New-ToolkitInputError 'productAllowListB64 is empty: refusing to run without an explicit list of products.')
    }
    $encoded = $Value.Trim()
    if ($encoded -cnotmatch '^[A-Za-z0-9+/]+={0,2}$' -or ($encoded.Length % 4) -ne 0) {
        throw (New-ToolkitInputError 'productAllowListB64 is not standard padded base64 (A-Z a-z 0-9 + / =).')
    }
    try {
        $bytes = [Convert]::FromBase64String($encoded)
        # throwOnInvalidBytes: a stray non-UTF-8 byte must fail here, not decode to U+FFFD and
        # then quietly match nothing.
        $utf8 = New-Object System.Text.UTF8Encoding -ArgumentList $false, $true
        $decoded = $utf8.GetString($bytes)
    } catch {
        throw (New-ToolkitInputError "productAllowListB64 does not decode to UTF-8 text: $($_.Exception.Message)")
    }
    $list = New-Object System.Collections.Generic.List[string]
    foreach ($title in $decoded.Split([char]'|')) {
        if ([string]::IsNullOrWhiteSpace($title)) {
            throw (New-ToolkitInputError "productAllowListB64 decodes to an empty product title ('$decoded').")
        }
        if (-not $list.Contains($title)) { $list.Add($title) }
    }
    return , $list.ToArray()
}

function Resolve-SoftwareRemediationRequest {
    <#
        The whole input contract in one pure function: argument line + environment in, a
        validated request out, or an ArgumentException. Defaults are the safe ones: a missing
        rebootBehavior means Never and a missing dryRun means true (report only).
    #>
    param([AllowNull()][object[]]$ArgumentList, [AllowNull()][hashtable]$Environment)
    $parsed = ConvertFrom-ToolkitArgumentLine -ArgumentList $ArgumentList -AllowedKeys (Get-SoftwareParameterKeys)
    $parsed = Merge-ToolkitEnvironment -Parsed $parsed -Environment $Environment -AllowedKeys (Get-SoftwareParameterKeys)
    if (-not $parsed.ContainsKey('productAllowListB64')) {
        throw (New-ToolkitInputError 'productAllowListB64 was not provided on the argument line or as a script variable.')
    }
    $reboot = 'Never'
    if ($parsed.ContainsKey('rebootBehavior')) { $reboot = ConvertTo-ToolkitRebootBehavior $parsed['rebootBehavior'] }
    $dryRun = $true
    if ($parsed.ContainsKey('dryRun')) { $dryRun = ConvertTo-ToolkitBoolean -Name 'dryRun' -Value $parsed['dryRun'] }
    return [pscustomobject]@{
        Products       = [string[]](ConvertFrom-ProductAllowList $parsed['productAllowListB64'])
        RebootBehavior = $reboot
        DryRun         = $dryRun
    }
}

function Test-IsElevated {
    $principal = New-Object Security.Principal.WindowsPrincipal([Security.Principal.WindowsIdentity]::GetCurrent())
    return $principal.IsInRole([Security.Principal.WindowsBuiltInRole]::Administrator)
}

#region install-hook - REPLACE THIS FUNCTION
function Install-SelectedProduct {
    <#
        Installs (or, with -DryRun, only evaluates) ONE product, named by its NinjaOne patch title
        such as "Google Chrome 141.0.7390.55". Return a hashtable:

            @{ State = '<state>'; RebootRequired = $false; Detail = '<one line for the log>' }

        State is one of:
            Installed         - this call installed it
            AlreadyInstalled  - the device already has this version or newer
            WouldInstall      - dry run only: an install would be attempted
            NotOffered        - no package matches this title on this device
            Failed            - an install was attempted and failed
            NotImplemented    - there is no install mechanism (the shipped default)

        Rules for a replacement:
          * Map the title to a package id with an explicit, reviewed table - never a fuzzy search -
            and return NotOffered for a title the table does not name.
          * With -DryRun, change nothing on the device.
          * Throwing is treated as an environment error (exit 5) for the whole run.
    #>
    param([Parameter(Mandatory = $true)][string]$Title, [bool]$DryRun)
    return @{
        State          = 'NotImplemented'
        RebootRequired = $false
        Detail         = 'no install mechanism configured - replace Install-SelectedProduct in this script'
    }
}
#endregion install-hook

function Invoke-SoftwareRemediation {
    param([AllowNull()][object[]]$ScriptArguments)

    try {
        $request = Resolve-SoftwareRemediationRequest -ArgumentList $ScriptArguments -Environment (Get-ToolkitEnvironment -AllowedKeys (Get-SoftwareParameterKeys))
    } catch [System.ArgumentException] {
        Write-Host "ERROR (bad input): $($_.Exception.Message)"
        Write-Host 'Expected: productAllowListB64=<base64 of titles joined by |> rebootBehavior=<Never|Auto> dryRun=<true|false>'
        return 1
    }

    $mode = 'INSTALL'
    if ($request.DryRun) { $mode = 'DRY RUN - nothing will be installed' }
    Write-Host "Mode: $mode"
    Write-Host "Products ($($request.Products.Count)):"
    foreach ($title in $request.Products) { Write-Host "  - $title" }
    Write-Host "rebootBehavior: $($request.RebootBehavior)"

    if (-not $request.DryRun -and -not (Test-IsElevated)) {
        Write-Host 'ERROR (environment): installing software needs an elevated session. Run as SYSTEM.'
        return 5
    }

    $known = @('Installed', 'AlreadyInstalled', 'WouldInstall', 'NotOffered', 'Failed', 'NotImplemented')
    $states = @()
    $rebootPending = $false
    foreach ($title in $request.Products) {
        try {
            $outcome = Install-SelectedProduct -Title $title -DryRun $request.DryRun
        } catch {
            Write-Host "ERROR (environment): install hook threw for '$title': $($_.Exception.Message)"
            return 5
        }
        $state = [string]$outcome.State
        if ($known -notcontains $state) {
            Write-Host "FAILED           '$title' : install hook returned unknown state '$state'."
            $state = 'Failed'
        } elseif ($request.DryRun -and $state -eq 'Installed') {
            # A hook that installs during a dry run is a bug worth surfacing, not a success.
            Write-Host "FAILED           '$title' : install hook reported Installed during a dry run."
            $state = 'Failed'
        } else {
            Write-Host ("{0,-16} '{1}' : {2}" -f $state.ToUpperInvariant(), $title, $outcome.Detail)
        }
        if ($outcome.RebootRequired -and -not $request.DryRun) { $rebootPending = $true }
        $states += $state
    }

    $rebootScheduled = $false
    if ($rebootPending) {
        if ($request.RebootBehavior -eq 'Auto') {
            # A delayed restart lets this script exit and the NinjaOne agent report the result
            # before the machine goes down.
            & shutdown.exe /r /t 60 /d p:4:1/c 'NinjaOne Patch Toolkit: restarting to finish installing software updates.' | Out-Host
            if ($LASTEXITCODE -eq 0) {
                $rebootScheduled = $true
                Write-Host 'REBOOT           : restart scheduled in 60 seconds (rebootBehavior=Auto).'
            } else {
                Write-Host "REBOOT           : a restart is required but shutdown.exe failed (exit $LASTEXITCODE)."
            }
        } else {
            Write-Host 'REBOOT           : a restart is required to finish; not restarting because rebootBehavior=Never.'
        }
    }

    $exitCode = Get-ToolkitExitCode -States $states -RebootPending $rebootPending -RebootScheduled $rebootScheduled -DryRun $request.DryRun
    if ($exitCode -eq 10) {
        Write-Host 'Nothing was installed: this reference script has no install mechanism. See remediation/README.md.'
    }
    Write-Host "Result: exit $exitCode"
    return $exitCode
}

if ($MyInvocation.InvocationName -ne '.') {
    # Select the last object so a stray pipeline write can never turn the exit code into an array.
    $code = Invoke-SoftwareRemediation -ScriptArguments $args | Select-Object -Last 1
    exit ([int]$code)
}
