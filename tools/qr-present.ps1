# Show qr_scanner_module's codes in this console: it draws them, this only prints them, through plain
# logosctl. While this runs, the module's helper reads the camera without a window. A request is shown
# with the camera off: press Enter once the wallet shows its answer, and again to see the request
# again. Ctrl-C to stop. $env:QR_PRESENT_SIZE = "rows cols" draws within at most that, for a console
# so big that its codes are too dense for the wallet's camera; the codes then shrink to fit.
#   tools\qr-present.ps1 [-Logosctl logosctl.exe]
param([string]$Logosctl = "logosctl")
$cap = @($env:QR_PRESENT_SIZE -split '\s+' | Where-Object { $_ } | ForEach-Object { [int]$_ })
$ErrorActionPreference = "Stop"
[Console]::OutputEncoding = [System.Text.Encoding]::UTF8
$h = (& $Logosctl --human call qr_scanner_module presenter_attach | Out-String).Trim()
[Console]::CursorVisible = $false
Clear-Host
try {
    while ($true) {
        $rows = [Console]::WindowHeight
        $cols = [Console]::WindowWidth
        if ($cap.Count -eq 2) { $rows = [Math]::Min($rows, $cap[0]); $cols = [Math]::Min($cols, $cap[1]) }
        $out = & $Logosctl --human call qr_scanner_module presenter_render "str:$h" $cols $rows | Out-String
        if ($LASTEXITCODE -ne 0) { break }
        [Console]::SetCursorPosition(0, 0)
        [Console]::Write($out.TrimEnd("`r", "`n"))
        [Console]::Write("$([char]27)[J")
        Start-Sleep -Milliseconds 200
        $pressed = $false
        while ([Console]::KeyAvailable) {
            [void][Console]::ReadKey($true)
            $pressed = $true
        }
        if ($pressed) {
            & $Logosctl call qr_scanner_module presenter_press "str:$h" | Out-Null
        }
    }
} finally {
    & $Logosctl call qr_scanner_module presenter_detach "str:$h" | Out-Null
    [Console]::Write("$([char]27)[0m")
    [Console]::CursorVisible = $true
}
