[CmdletBinding()]
param(
    [ValidateSet('Debug','Release')][string]$Configuration = 'Release',
    [string]$Runtime = 'win-x64',
    [switch]$Install
)

$ErrorActionPreference = 'Stop'

$project = Join-Path $PSScriptRoot 'LocalBox.Tui\LocalBox.Tui.csproj'
$publishRoot = Join-Path $PSScriptRoot "publish\$Runtime"

dotnet publish $project -c $Configuration -r $Runtime --self-contained false -o $publishRoot

if ($Install) {
    $target = Join-Path $HOME '.local-llm\bin'
    New-Item -ItemType Directory -Force -Path $target | Out-Null
    Copy-Item -Path (Join-Path $publishRoot '*') -Destination $target -Recurse -Force
    Write-Host "Installed LocalBox.Tui to $target"
}
else {
    Write-Host "Published LocalBox.Tui to $publishRoot"
}
