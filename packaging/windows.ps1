# Build release binaries locally on Windows. WebView2 must be installed (included on GitHub Actions images).
# No .app, Flatpak, or MSI.
#
#   $env:TINYCHAT_TELEGRAM_API_ID = '...'
#   $env:TINYCHAT_TELEGRAM_API_HASH = '...'
#   cargo build --release -p tinyclient --features tinyclient/gui -p chatd -p mediad -p login
#
# Binaries: target\release\{tinyclient,chatd,mediad,tinyclient-login}.exe
Write-Host "See comments in packaging/windows.ps1 for the cargo build command."
