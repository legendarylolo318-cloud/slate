# Usage: collect.ps1 <out-dir>  — copies slate.exe, then every non-system DLL
# it needs (walked recursively with dumpbin), plus GStreamer plugins + scanner.
param([Parameter(Mandatory)][string]$Out)
$ErrorActionPreference = "Stop"
$root = $env:GSTREAMER_1_0_ROOT_MSVC_X86_64
New-Item -ItemType Directory -Force $Out, "$Out\gst-plugins" | Out-Null
Copy-Item target\release\slate.exe $Out
$vs = & "${env:ProgramFiles(x86)}\Microsoft Visual Studio\Installer\vswhere.exe" -latest -property installationPath
$dumpbin = (Get-ChildItem "$vs\VC\Tools\MSVC" -Recurse -Filter dumpbin.exe | Where-Object { $_.FullName -like "*Hostx64\x64*" } | Select-Object -First 1).FullName
$plugins = "gstcoreelements","gstplayback","gsttypefindfunctions","gstaudioconvert","gstaudioresample","gstvolume","gstautodetect","gstapp","gstvideoconvertscale","gstvideoparsersbad","gstisomp4","gstmatroska","gstid3demux","gstaudioparsers","gstflac","gstvorbis","gstopus","gstogg","gstlame","gstmpg123","gstlibav","gsthls","gstadaptivedemux2","gstdash","gstsoup","gsttcp","gstwasapi2","gstwasapi","gstreplaygain","gstequalizer","gstaudiofx","gstvolume","gstopengl","gstd3d11","gstrawparse","gstdebugutilsbad","gstdeinterlace","gstvideoscale","gstvideorate","gsttypefindfunctions","gstgio","gstpbtypes","gstcurl","gstflac"
foreach ($p in $plugins | Select-Object -Unique) {
  $f = "$root\lib\gstreamer-1.0\$p.dll"
  if (Test-Path $f) { Copy-Item $f "$Out\gst-plugins" }
}
# The scanner runs as its own process; next to slate.exe it finds the GStreamer DLLs.
Copy-Item "$root\libexec\gstreamer-1.0\gst-plugin-scanner.exe" $Out
# GIO loads its TLS backend (HTTPS) at runtime from GIO_MODULE_DIR, which
# main.rs sets, so the dumpbin walk below cannot see it.
# Where gstsoup links libsoup directly the dumpbin walk below finds it anyway;
# copy any libsoup DLL so a dlopen-ing build works too. Whether HTTPS really
# works is decided by the smoke test's `slate --self-test --network`.
Get-ChildItem "$root\bin" -Filter "*soup*.dll" | ForEach-Object { Write-Host "libsoup: $($_.Name)"; Copy-Item $_.FullName $Out }
New-Item -ItemType Directory -Force "$Out\gio\modules" | Out-Null
Get-ChildItem "$root\lib\gio\modules" -Filter "*.dll" -ErrorAction SilentlyContinue | ForEach-Object { Write-Host "gio module: $($_.Name)"; Copy-Item $_.FullName "$Out\gio\modules" }
# VC++ runtime: GStreamer's DLLs link it dynamically and a clean machine may lack it.
$crt = Get-ChildItem "$vs\VC\Redist\MSVC" -Recurse -Directory -Filter Microsoft.VC14*.CRT | Where-Object { $_.FullName -like "*\x64\*" -and $_.FullName -notlike "*onecore*" -and $_.FullName -notlike "*debug_nonredist*" } | Select-Object -First 1
if ($crt) { Copy-Item "$($crt.FullName)\*.dll" $Out }
$seen = @{}
$queue = [System.Collections.Queue]::new()
Get-ChildItem $Out -Recurse -Include *.exe,*.dll | ForEach-Object { $queue.Enqueue($_.FullName) }
while ($queue.Count) {
  $file = $queue.Dequeue()
  foreach ($line in & $dumpbin /dependents $file) {
    if ($line -match '^\s+(\S+\.dll)\s*$') {
      $dll = $Matches[1].ToLower()
      if ($seen[$dll]) { continue }
      $seen[$dll] = $true
      $src = Get-ChildItem "$root\bin\$dll" -ErrorAction SilentlyContinue
      if ($src) { Copy-Item $src.FullName $Out; $queue.Enqueue("$Out\$dll") }
    }
  }
}
$missing = foreach ($f in Get-ChildItem $Out -Recurse -Include *.exe,*.dll) {
  foreach ($line in & $dumpbin /dependents $f.FullName) {
    if ($line -match '^\s+(\S+\.dll)\s*$') {
      $d = $Matches[1]
      if (-not (Test-Path "$Out\$d") -and -not (Test-Path "$env:SystemRoot\System32\$d") -and $d -notmatch '^(api-ms-|ext-ms-|ucrtbase)') { "$($f.Name) -> $d" }
    }
  }
}
if ($missing) { Write-Error ("Missing DLLs:`n" + ($missing | Sort-Object -Unique | Out-String)) }
