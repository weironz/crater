# Cross-compile a static Linux binary of crater from Windows.
#
# Produces a fully static (musl) ELF that runs on any x86_64 Linux with zero
# runtime dependencies — verified with `ldd` => "not a dynamic executable".
#
# Prereqs (one-time):
#   winget install -e --id zig.zig
#   cargo install cargo-zigbuild
#   rustup target add x86_64-unknown-linux-musl
#
# Why zig: the dependency tree pulls in `ring` (C/asm), so a plain
# `cargo build --target ...-musl` needs a C cross-compiler. cargo-zigbuild uses
# Zig as the C compiler + linker, which Just Works on Windows.
#
# Usage:
#   pwsh scripts/build-linux.ps1                 # x86_64 musl (default)
#   pwsh scripts/build-linux.ps1 -Arch aarch64   # aarch64 musl
param(
    [ValidateSet('x86_64', 'aarch64')]
    [string]$Arch = 'x86_64'
)

$ErrorActionPreference = 'Stop'
$target = "$Arch-unknown-linux-musl"
$repo = Split-Path -Parent $PSScriptRoot

# Put a winget-installed zig on PATH if not already visible.
if (-not (Get-Command zig -ErrorAction SilentlyContinue)) {
    $zig = Get-ChildItem "$env:LOCALAPPDATA\Microsoft\WinGet\Packages" -Recurse -Filter zig.exe -ErrorAction SilentlyContinue |
        Select-Object -First 1 -ExpandProperty DirectoryName
    if ($zig) { $env:Path = "$zig;$env:Path" }
    else { throw "zig not found. Run: winget install -e --id zig.zig" }
}

Write-Host "Building crater for $target ..." -ForegroundColor Cyan
Push-Location $repo
try {
    cargo zigbuild --release --target $target
    if ($LASTEXITCODE -ne 0) { throw "cargo zigbuild failed ($LASTEXITCODE)" }

    New-Item -ItemType Directory -Force "$repo\dist" | Out-Null
    $out = "$repo\dist\crater-linux-$Arch"
    Copy-Item "$repo\target\$target\release\crater" $out -Force
    $mb = [math]::Round((Get-Item $out).Length / 1MB, 2)
    Write-Host "OK -> dist\crater-linux-$Arch  ($mb MB, static musl)" -ForegroundColor Green
}
finally {
    Pop-Location
}
