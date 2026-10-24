<#
.SYNOPSIS
Prepare a private Minecraft 26.3 resource archive for Horizon on iPad.
.DESCRIPTION
This wrapper forwards arguments to prepare_ipad_resources.py. The Minecraft installation
and supplied server JAR are read-only inputs; output is restricted to this checkout's .local/.
Examples:
  .\tools\prepare_ipad_resources.ps1 --server-jar C:\Downloads\server.jar
  .\tools\prepare_ipad_resources.ps1 --prepared .local\horizon-client\26.3
#>
$ErrorActionPreference = 'Stop'
$script = Join-Path $PSScriptRoot 'prepare_ipad_resources.py'
$selectedPython = $null
$pythonPrefix = @()
$pythonLauncher = Get-Command py -ErrorAction SilentlyContinue
if ($pythonLauncher) {
    try {
        $version = (& $pythonLauncher.Source -3 --version 2>&1 | Out-String).Trim()
        if ($LASTEXITCODE -eq 0 -and $version -match '^Python 3\.') {
            $selectedPython = $pythonLauncher.Source
            $pythonPrefix = @('-3')
        }
    } catch {
        # Try the ordinary Python command below; Windows can ship a launcher without a runtime.
    }
}
if (-not $selectedPython) {
    $python = Get-Command python -ErrorAction SilentlyContinue
    if ($python) {
        try {
            $version = (& $python.Source --version 2>&1 | Out-String).Trim()
            if ($LASTEXITCODE -eq 0 -and $version -match '^Python 3\.') {
                $selectedPython = $python.Source
            }
        } catch {
            # Report the same actionable error when the command is only a broken app alias.
        }
    }
}
if (-not $selectedPython) {
    throw 'Python 3 is required. Install Python 3 and rerun this script.'
}
& $selectedPython @pythonPrefix $script @args
exit $LASTEXITCODE
