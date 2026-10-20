# Runs the remediation scripts' Pester suites and exits non-zero on any failure.
# `just pester` (PowerShell 7) and `just pester powershell` (Windows PowerShell 5.1, the
# interpreter NinjaOne runs the scripts under) both call this, and CI runs both. A script
# file rather than an inline -Command: Windows PowerShell 5.1 drops the quotes inside an
# argument it passes to a native command, so an inline configuration did not survive the
# trip through just's windows-shell.
$ErrorActionPreference = 'Stop'

# Imported explicitly rather than by `#Requires -Modules`: with that line, `pwsh -File`
# exited 0 whatever the script's own `exit` said, so a failing run passed CI.
Import-Module Pester -MinimumVersion 5.0.0

$config = New-PesterConfiguration
$config.Run.Path = Join-Path (Split-Path -Parent $PSScriptRoot) 'remediation/tests'
$config.Run.PassThru = $true
$config.Output.Verbosity = 'Detailed'
$result = Invoke-Pester -Configuration $config
if ($result.Result -ne 'Passed') { exit 1 }
