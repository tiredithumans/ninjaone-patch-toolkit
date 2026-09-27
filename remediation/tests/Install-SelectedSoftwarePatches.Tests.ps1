#Requires -Modules @{ ModuleName = 'Pester'; ModuleVersion = '5.0.0' }
# Pester 5 suite for the input contract of Install-SelectedSoftwarePatches.ps1. Dot-sourcing the
# script only defines its functions. Run with
#   Invoke-Pester -Path remediation/tests

BeforeDiscovery {
    $fixture = Get-Content -Raw -Encoding UTF8 -Path (Join-Path $PSScriptRoot 'fixtures/parameter-contract.json') | ConvertFrom-Json
    $contractCases = @($fixture.cases | Where-Object { $_.kind -eq 'SOFTWARE_PATCH_REMEDIATE' } | ForEach-Object {
            @{ Name = $_.name; Parameters = $_.parameters; Expect = $_.expect }
        })
}

BeforeAll {
    $remediationDir = Split-Path -Parent $PSScriptRoot
    $scriptPath = Join-Path $remediationDir 'Install-SelectedSoftwarePatches.ps1'
    . $scriptPath

    function Split-AsNinjaOne([string]$Line) { return , @($Line -split ' ') }
    function ConvertTo-Base64Utf8([string]$Text) { return [Convert]::ToBase64String([System.Text.Encoding]::UTF8.GetBytes($Text)) }
    function Get-ParserRegion([string]$Path) {
        $text = [System.IO.File]::ReadAllText($Path)
        $match = [regex]::Match($text, '(?s)#region toolkit-parameter-parser.*?#endregion toolkit-parameter-parser')
        if (-not $match.Success) { throw "no parser region in $Path" }
        return $match.Value -replace "`r`n", "`n"
    }

    # Compares two lists as one string: Should -Be on a one-element array unrolls the actual
    # value, so element-by-element comparison is not reliable across Pester 5 versions.
    function Join-List([object[]]$Items) { return (@($Items) | ForEach-Object { [string]$_ }) -join [char]0x1f }
}

Describe 'Install-SelectedSoftwarePatches.ps1 source' {
    It 'parses without errors' {
        $tokens = $null; $errors = $null
        [void][System.Management.Automation.Language.Parser]::ParseFile($scriptPath, [ref]$tokens, [ref]$errors)
        $errors | Should -BeNullOrEmpty
    }

    It 'is pure ASCII, so Windows PowerShell 5.1 reads it the same without a BOM' {
        $bytes = [System.IO.File]::ReadAllBytes($scriptPath)
        @($bytes | Where-Object { $_ -gt 127 }).Count | Should -Be 0
    }

    It 'carries the same parameter parser as Install-SelectedWindowsUpdates.ps1' {
        Get-ParserRegion $scriptPath |
            Should -BeExactly (Get-ParserRegion (Join-Path $remediationDir 'Install-SelectedWindowsUpdates.ps1'))
    }
}

Describe 'the toolkit parameter contract (fixture shared with build_parameters)' {
    It '<Name>' -ForEach $contractCases {
        $argumentList = Split-AsNinjaOne $Parameters
        if ($Expect.error) {
            { Resolve-SoftwareRemediationRequest -ArgumentList $argumentList -Environment @{} } |
                Should -Throw -ExceptionType ([System.ArgumentException])
        } else {
            $request = Resolve-SoftwareRemediationRequest -ArgumentList $argumentList -Environment @{}
            Join-List ($request.Products) | Should -BeExactly (Join-List @($Expect.products))
            $request.RebootBehavior | Should -BeExactly $Expect.rebootBehavior
            $request.DryRun | Should -Be $Expect.dryRun
        }
    }
}

Describe 'ConvertFrom-ProductAllowList' {
    It 'keeps titles verbatim, including surrounding spaces' {
        Join-List (ConvertFrom-ProductAllowList (ConvertTo-Base64Utf8 ' Tool A |Tool B')) | Should -BeExactly (Join-List @(' Tool A ', 'Tool B'))
    }

    It 'drops a repeated title and keeps the first-seen order' {
        Join-List (ConvertFrom-ProductAllowList (ConvertTo-Base64Utf8 'B|A|B')) | Should -BeExactly (Join-List @('B', 'A'))
    }

    It 'rejects <Why>' -ForEach @(
        @{ Why = 'an empty value'; Value = '' }
        @{ Why = 'characters outside the base64 alphabet'; Value = 'not-base64!' }
        @{ Why = 'the URL-safe alphabet'; Value = 'UGFpbnQuTkVUIDUuMC4xMyB-IGJldGE_' }
        @{ Why = 'missing padding'; Value = 'UGFpbnQ' }
        @{ Why = 'bytes that are not UTF-8'; Value = '/w==' }
        @{ Why = 'an empty title between separators'; Value = 'YXx8Yg==' }
        @{ Why = 'a whitespace-only title'; Value = 'YXwgfGI=' }
    ) {
        { ConvertFrom-ProductAllowList $Value } | Should -Throw -ExceptionType ([System.ArgumentException])
    }
}

Describe 'Resolve-SoftwareRemediationRequest' {
    It 'reads script variables from the environment when the argument line is empty' {
        $request = Resolve-SoftwareRemediationRequest -ArgumentList @() -Environment @{
            productAllowListB64 = (ConvertTo-Base64Utf8 '7-Zip'); dryRun = 'false'
        }
        Join-List ($request.Products) | Should -BeExactly (Join-List @('7-Zip'))
        $request.DryRun | Should -BeFalse
        $request.RebootBehavior | Should -BeExactly 'Never'
    }

    It 'rejects a KB list sent to the software script' {
        { Resolve-SoftwareRemediationRequest -ArgumentList @('kbAllowList=5040434') -Environment @{} } |
            Should -Throw -ExceptionType ([System.ArgumentException])
    }
}

Describe 'Invoke-SoftwareRemediation' {
    BeforeAll {
        $oneProduct = "productAllowListB64=$(ConvertTo-Base64Utf8 'Google Chrome 141.0.7390.55')"
    }

    It 'exits 10 with the shipped hook, even on a dry run' {
        Invoke-SoftwareRemediation -ScriptArguments @($oneProduct, 'dryRun=true') 6>$null | Should -Be 10
    }

    It 'exits 1 on bad input without calling the hook' {
        Mock Install-SelectedProduct { throw 'must not be called' }
        Invoke-SoftwareRemediation -ScriptArguments @('productAllowListB64=', 'dryRun=true') 6>$null | Should -Be 1
        Should -Invoke Install-SelectedProduct -Times 0
    }

    It 'exits 5 when a real install is not elevated' {
        Mock Test-IsElevated { $false }
        Mock Install-SelectedProduct { throw 'must not be called' }
        Invoke-SoftwareRemediation -ScriptArguments @($oneProduct, 'dryRun=false') 6>$null | Should -Be 5
        Should -Invoke Install-SelectedProduct -Times 0
    }

    It 'exits 4 when an install needs a reboot and rebootBehavior=Never' {
        Mock Test-IsElevated { $true }
        Mock Install-SelectedProduct { @{ State = 'Installed'; RebootRequired = $true; Detail = 'ok' } }
        Invoke-SoftwareRemediation -ScriptArguments @($oneProduct, 'dryRun=false', 'rebootBehavior=Never') 6>$null | Should -Be 4
    }

    It 'exits 3 when a hook claims Installed during a dry run' {
        Mock Install-SelectedProduct { @{ State = 'Installed'; RebootRequired = $false; Detail = 'oops' } }
        Invoke-SoftwareRemediation -ScriptArguments @($oneProduct, 'dryRun=true') 6>$null | Should -Be 3
    }

    It 'exits 3 when a hook returns an unknown state' {
        Mock Install-SelectedProduct { @{ State = 'Done'; RebootRequired = $false; Detail = '' } }
        Invoke-SoftwareRemediation -ScriptArguments @($oneProduct, 'dryRun=true') 6>$null | Should -Be 3
    }

    It 'exits 0 when a dry run finds every product actionable' {
        Mock Install-SelectedProduct { @{ State = 'WouldInstall'; RebootRequired = $false; Detail = 'would upgrade' } }
        Invoke-SoftwareRemediation -ScriptArguments @($oneProduct, 'dryRun=true') 6>$null | Should -Be 0
    }
}
