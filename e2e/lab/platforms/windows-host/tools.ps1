# Installs what the lab needs on winbuild that is not there, with no administrator: portable
# ffmpeg (recorder) and protoc (Signal's protobufs) under <Work>\tools. Safe to run again.
# Prints "ffmpeg=<path>" and "protoc=<path>" last.
param([string]$Work = 'C:\lab')
$ErrorActionPreference = 'Stop'
$ProgressPreference = 'SilentlyContinue'
[Net.ServicePointManager]::SecurityProtocol = [Net.SecurityProtocolType]::Tls12
$tools = Join-Path $Work 'tools'
New-Item -ItemType Directory -Force $tools | Out-Null

$ffmpeg = Join-Path $tools 'ffmpeg\bin\ffmpeg.exe'
if (-not (Test-Path $ffmpeg)) {
    $zip = Join-Path $tools 'ffmpeg.zip'
    Invoke-WebRequest -UseBasicParsing 'https://www.gyan.dev/ffmpeg/builds/ffmpeg-release-essentials.zip' -OutFile $zip
    # gyan.dev publishes the zip's SHA-256 beside it.
    $want = (Invoke-WebRequest -UseBasicParsing 'https://www.gyan.dev/ffmpeg/builds/ffmpeg-release-essentials.zip.sha256').Content.Trim().Split()[0]
    $got = (Get-FileHash $zip -Algorithm SHA256).Hash
    if ($got -ne $want) { Remove-Item $zip; throw "ffmpeg zip sha256 $got, expected $want" }
    $tmp = Join-Path $tools 'ffmpeg-unpack'
    Remove-Item -Recurse -Force $tmp -ErrorAction SilentlyContinue
    Expand-Archive $zip -DestinationPath $tmp
    Move-Item (Get-ChildItem $tmp -Directory | Select-Object -First 1).FullName (Join-Path $tools 'ffmpeg')
    Remove-Item -Recurse -Force $tmp, $zip
    Write-Host "installed ffmpeg (sha256 $got)"
}

# The version CI uses (packaging/ci/protoc.sh). GitHub publishes no checksum for it.
$protoc = Join-Path $tools 'protoc\bin\protoc.exe'
if (-not (Test-Path $protoc)) {
    $zip = Join-Path $tools 'protoc.zip'
    Invoke-WebRequest -UseBasicParsing 'https://github.com/protocolbuffers/protobuf/releases/download/v29.3/protoc-29.3-win64.zip' -OutFile $zip
    Expand-Archive $zip -DestinationPath (Join-Path $tools 'protoc')
    Remove-Item $zip
    Write-Host "installed protoc (sha256 $((Get-FileHash $protoc -Algorithm SHA256).Hash))"
}
& $ffmpeg -hide_banner -version | Select-Object -First 1
& $protoc --version
Write-Host "ffmpeg=$ffmpeg"
Write-Host "protoc=$protoc"
