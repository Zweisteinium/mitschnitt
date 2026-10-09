# Run a command inside the VS 2022 Build Tools environment, e.g.  .\dev.ps1 cargo test
#
# Why: this machine also has a Visual Studio 18 Insiders instance without its C++ libraries, and rustc
# picks the newest instance by itself (-> LNK1104 msvcrt.lib). Inside vcvars64 rustc uses the tools and
# library paths of the environment instead.
$ErrorActionPreference = "Stop"
$vcvars = & "${env:ProgramFiles(x86)}\Microsoft Visual Studio\Installer\vswhere.exe" `
    -products Microsoft.VisualStudio.Product.BuildTools -latest -find "VC\Auxiliary\Build\vcvars64.bat"
if (-not $vcvars) { throw "VS Build Tools with the C++ workload not found" }
cmd /c "`"$vcvars`" >nul 2>&1 && set" | ForEach-Object {
    if ($_ -match '^([^=]+)=(.*)$') { Set-Item -Path "env:$($matches[1])" -Value $matches[2] }
}
$env:Path = "$env:USERPROFILE\.cargo\bin;C:\Program Files\nodejs;$env:Path"
$ErrorActionPreference = "Continue"  # cargo/npm report progress on stderr; that's not an error
if ($args.Count -gt 0) { & $args[0] @($args | Select-Object -Skip 1); exit $LASTEXITCODE }
