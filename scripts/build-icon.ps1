# Pack the exported app-icon PNGs (assets/icon/png/corral-<size>.png, from the Figma file
# "Corral — Brand & Theme", page "App icon · FINAL") into assets/icon/corral.ico.
# Every entry is stored PNG-compressed (supported for all sizes since Windows Vista).
# 16/20/24/32 are hand-drawn on their own pixel grids; 40/48/64/256 are renders of the 256 master.
# build.rs embeds the .ico as RT_ICON/RT_GROUP_ICON; rerun this script after re-exporting the PNGs.
$ErrorActionPreference = 'Stop'
$projectRoot = Split-Path -Parent $PSScriptRoot
$iconDir = Join-Path $projectRoot 'assets\icon'
$sizes = 16, 20, 24, 32, 40, 48, 64, 256

$images = foreach ($size in $sizes) {
    $path = Join-Path $iconDir "png\corral-$size.png"
    $bytes = [IO.File]::ReadAllBytes($path)
    $width = [Net.IPAddress]::NetworkToHostOrder([BitConverter]::ToInt32($bytes, 16))
    $height = [Net.IPAddress]::NetworkToHostOrder([BitConverter]::ToInt32($bytes, 20))
    if ($bytes[0] -ne 0x89 -or $width -ne $size -or $height -ne $size) { throw "$path is not a ${size}x$size PNG" }
    [pscustomobject]@{ Size = $size; Bytes = $bytes }
}

$stream = [IO.MemoryStream]::new()
$writer = [IO.BinaryWriter]::new($stream)
$writer.Write([uint16]0)                # reserved
$writer.Write([uint16]1)                # type: icon
$writer.Write([uint16]$images.Count)
$offset = 6 + 16 * $images.Count
foreach ($image in $images) {
    $dimension = if ($image.Size -ge 256) { 0 } else { $image.Size }   # 0 means 256
    $writer.Write([byte]$dimension)     # width
    $writer.Write([byte]$dimension)     # height
    $writer.Write([byte]0)              # palette colours
    $writer.Write([byte]0)              # reserved
    $writer.Write([uint16]1)            # planes
    $writer.Write([uint16]32)           # bits per pixel
    $writer.Write([uint32]$image.Bytes.Length)
    $writer.Write([uint32]$offset)
    $offset += $image.Bytes.Length
}
foreach ($image in $images) { $writer.Write($image.Bytes) }
$writer.Flush()
$out = Join-Path $iconDir 'corral.ico'
[IO.File]::WriteAllBytes($out, $stream.ToArray())
Write-Output "Wrote $out ($($stream.Length) bytes, $($images.Count) images: $($sizes -join ', '))"
