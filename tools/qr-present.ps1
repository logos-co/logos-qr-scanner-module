# Show qr_scanner_module's codes in this console: it draws them, this only prints them, through plain
# logosctl. While this runs, the module's helper reads the camera without a window. Ctrl-C to stop.
#   tools\qr-present.ps1 [-Logosctl logosctl.exe]
param([string]$Logosctl = "logosctl")
$ErrorActionPreference = "Stop"
[Console]::OutputEncoding = [System.Text.Encoding]::UTF8
$h = (& $Logosctl --human call qr_scanner_module presenter_attach | Out-String).Trim()
[Console]::CursorVisible = $false
Clear-Host
try {
    while ($true) {
        $out = & $Logosctl --human call qr_scanner_module presenter_render "str:$h" ([Console]::WindowWidth) ([Console]::WindowHeight) | Out-String
        if ($LASTEXITCODE -ne 0) { break }
        [Console]::SetCursorPosition(0, 0)
        [Console]::Write($out.TrimEnd("`r", "`n"))
        [Console]::Write("$([char]27)[J")
        Start-Sleep -Milliseconds 200
    }
} finally {
    & $Logosctl call qr_scanner_module presenter_detach "str:$h" | Out-Null
    [Console]::Write("$([char]27)[0m")
    [Console]::CursorVisible = $true
}
