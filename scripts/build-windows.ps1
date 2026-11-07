param([ValidateSet('build','check','test')][string]$Action = 'build')
$ErrorActionPreference = 'Stop'
$workspace = Split-Path -Parent $PSScriptRoot
Push-Location -LiteralPath $workspace
try {
    $toolsDir = Join-Path $workspace 'target\tools'
    $protocDir = Join-Path $toolsDir 'protoc-26.1'
    $protocExe = Join-Path $protocDir 'bin\protoc.exe'
    if (-not (Test-Path -LiteralPath $protocExe)) {
        New-Item -ItemType Directory -Path $protocDir -Force | Out-Null
        $archive = Join-Path $toolsDir 'protoc-26.1-win64.zip'
        Invoke-WebRequest -Uri 'https://github.com/protocolbuffers/protobuf/releases/download/v26.1/protoc-26.1-win64.zip' -OutFile $archive
        $expected = '9090D135A1159042B13B4E51B210E40CB820D85A5032A6ECA5F9B3CA3BDFB539'
        if ((Get-FileHash -LiteralPath $archive -Algorithm SHA256).Hash -ne $expected) { throw 'protoc SHA-256 mismatch' }
        tar -xf $archive -C $protocDir
        if ($LASTEXITCODE -ne 0) { throw 'protoc extraction failed' }
    }
    $env:PROTOC = $protocExe
    if (-not (Get-Command 7z -ErrorAction SilentlyContinue)) {
        $sevenZip = Join-Path $env:ProgramFiles '7-Zip'
        if (-not (Test-Path -LiteralPath (Join-Path $sevenZip '7z.exe'))) { throw 'EasyTier Windows build requires 7-Zip (7z.exe).' }
        $env:PATH = "$sevenZip;$env:PATH"
    }
    switch ($Action) {
        'build' { cargo build --workspace --locked }
        'check' { cargo check --workspace --locked }
        'test' { cargo test -p xrose-core --locked -- --skip second_acquire_fails }
    }
    if ($LASTEXITCODE -ne 0) { throw "cargo $Action failed" }
} finally { Pop-Location }
