<#
.SYNOPSIS
  Write the winget manifests for a published Storecat release.

.DESCRIPTION
  Downloads the NSIS installer from the GitHub release for -Version, hashes it, and writes
  manifests/s/Storecat/Storecat/<Version>/ from the newest existing manifest folder.
  Then validate and submit:

    winget validate packaging\winget\manifests\s\Storecat\Storecat\<Version>
    winget install --manifest packaging\winget\manifests\s\Storecat\Storecat\<Version>
    wingetcreate submit packaging\winget\manifests\s\Storecat\Storecat\<Version>

.EXAMPLE
  .\packaging\winget\update-manifest.ps1 -Version 1.0.0
#>
param(
  [Parameter(Mandatory = $true)][string]$Version,
  [string]$Repo = "samcarmex/storecat"
)
$ErrorActionPreference = "Stop"

$root = Join-Path $PSScriptRoot "manifests\s\Storecat\Storecat"
$template = Get-ChildItem $root -Directory | Sort-Object { [version]$_.Name } | Select-Object -Last 1
$target = Join-Path $root $Version

$asset = "Storecat_${Version}_x64-setup.exe"
$url = "https://github.com/$Repo/releases/download/v$Version/$asset"
$tmp = Join-Path ([IO.Path]::GetTempPath()) $asset
Write-Host "Downloading $url"
Invoke-WebRequest -Uri $url -OutFile $tmp -UseBasicParsing
$sha = (Get-FileHash $tmp -Algorithm SHA256).Hash
Remove-Item $tmp

New-Item -ItemType Directory -Force $target | Out-Null
foreach ($f in Get-ChildItem $template.FullName -Filter *.yaml) {
  $text = Get-Content $f.FullName -Raw
  $text = $text -replace [regex]::Escape($template.Name), $Version
  $text = $text -replace "InstallerSha256: [0-9A-Fa-f]{64}", "InstallerSha256: $sha"
  $text = $text -replace "ReleaseDate: \d{4}-\d{2}-\d{2}", ("ReleaseDate: " + (Get-Date -Format "yyyy-MM-dd"))
  Set-Content -Path (Join-Path $target $f.Name) -Value $text -Encoding utf8 -NoNewline
}
Write-Host "Wrote $target (SHA256 $sha)"
