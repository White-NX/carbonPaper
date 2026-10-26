[CmdletBinding()]
param(
    [string]$Version,
    [string]$ConfigPath,
    [string]$BackgroundPath,
    [string]$OutputPath,
    [string]$RootDir
)

Set-StrictMode -Version Latest
$ErrorActionPreference = "Stop"

Add-Type -AssemblyName System.Drawing

if ([string]::IsNullOrWhiteSpace($RootDir)) {
    $RootDir = (Resolve-Path (Join-Path $PSScriptRoot "..")).Path
} else {
    $RootDir = (Resolve-Path -LiteralPath $RootDir).Path
}

function Resolve-BannerPath {
    param([string]$Path)

    if ([System.IO.Path]::IsPathRooted($Path)) {
        return [System.IO.Path]::GetFullPath($Path)
    }
    return [System.IO.Path]::GetFullPath((Join-Path $RootDir $Path))
}

if ([string]::IsNullOrWhiteSpace($Version)) {
    $package = Get-Content -Raw -LiteralPath (Join-Path $RootDir "package.json") | ConvertFrom-Json
    $Version = [string]$package.version
}
$Version = $Version.TrimStart("v")
if ($Version -notmatch '^\d+\.\d+\.\d+(?:-[0-9A-Za-z.-]+)?(?:\+[0-9A-Za-z.-]+)?$') {
    throw "Invalid release version: $Version"
}

if ([string]::IsNullOrWhiteSpace($ConfigPath)) {
    $ConfigPath = "docs/releases/v$Version.banner.json"
}
$configFile = Resolve-BannerPath $ConfigPath
$config = Get-Content -Raw -LiteralPath $configFile | ConvertFrom-Json

foreach ($field in @("version", "codename", "category", "headline", "background", "font", "focus", "colors")) {
    if (-not $config.PSObject.Properties[$field]) {
        throw "Banner config is missing '$field': $configFile"
    }
}
if ([string]$config.version -ne $Version) {
    throw "Config version '$($config.version)' does not match requested version '$Version'."
}
foreach ($field in @("category", "headline", "font")) {
    if ([string]::IsNullOrWhiteSpace([string]$config.$field)) {
        throw "Banner config '$field' must not be empty."
    }
}
foreach ($field in @("category", "headline", "codename")) {
    if ([string]$config.$field -match '[\r\n]') {
        throw "Banner config '$field' must fit on one line."
    }
}
foreach ($axis in @("x", "y")) {
    if (-not $config.focus.PSObject.Properties[$axis] -or
        $null -eq $config.focus.$axis -or
        ($config.focus.$axis -isnot [ValueType]) -or
        $config.focus.$axis -is [bool] -or
        [double]::IsNaN([double]$config.focus.$axis) -or
        [double]::IsInfinity([double]$config.focus.$axis) -or
        [double]$config.focus.$axis -lt 0 -or [double]$config.focus.$axis -gt 1) {
        throw "Banner focus.$axis must be a number between 0 and 1."
    }
}
foreach ($field in @("category", "version", "headline", "rule")) {
    if (-not $config.colors.PSObject.Properties[$field] -or
        [string]$config.colors.$field -notmatch '^#[0-9a-fA-F]{6}$') {
        throw "Banner colors.$field must use #RRGGBB."
    }
}

if ([string]::IsNullOrWhiteSpace($BackgroundPath)) {
    $BackgroundPath = [string]$config.background
}
$backgroundFile = Resolve-BannerPath $BackgroundPath
if ([string]::IsNullOrWhiteSpace($OutputPath)) {
    $OutputPath = ".version-banner.local/output/v$Version-banner.png"
}
$outputFile = Resolve-BannerPath $OutputPath
if ([System.IO.Path]::GetExtension($outputFile) -ine ".png") {
    throw "Banner output must be a PNG file."
}
if ($outputFile -ieq $backgroundFile) {
    throw "Banner output must use a different path from its background."
}

# v0.8.5's original release attachment is exactly 1910 x 395 pixels.
$width = 1910
$height = 395
$renderScale = 2
$fontFamily = [System.Drawing.FontFamily]::new([string]$config.font)
$background = $null
$canvas = $null
$graphics = $null
$result = $null
$resultGraphics = $null
$ruleBrush = $null
$format = [System.Drawing.StringFormat]::GenericTypographic.Clone()
$format.FormatFlags = $format.FormatFlags -bor [System.Drawing.StringFormatFlags]::NoWrap
$format.Trimming = [System.Drawing.StringTrimming]::None

function Draw-BannerLine {
    param(
        [string]$Text,
        [float]$Y,
        [float]$FontSize,
        [float]$MinimumFontSize,
        [System.Drawing.FontStyle]$Style,
        [string]$Color
    )

    $lineFont = $null
    $lineBrush = $null
    try {
        while ($true) {
            $lineFont = [System.Drawing.Font]::new($fontFamily, $FontSize, $Style, [System.Drawing.GraphicsUnit]::Pixel)
            $measurement = $graphics.MeasureString($Text, $lineFont, [int]::MaxValue, $format)
            if ($measurement.Width -le 1100) { break }
            $lineFont.Dispose()
            $lineFont = $null
            $FontSize -= 1
            if ($FontSize -lt $MinimumFontSize) {
                throw "Banner text is too long. Shorten '$Text' so it fits the text area."
            }
        }
        $lineBrush = [System.Drawing.SolidBrush]::new([System.Drawing.ColorTranslator]::FromHtml($Color))
        $graphics.DrawString($Text, $lineFont, $lineBrush, [System.Drawing.PointF]::new(146, $Y), $format)
        return [ordered]@{ text = $Text; font_size_px = $FontSize; x = 146; y = $Y; width_px = [math]::Round($measurement.Width, 2) }
    } finally {
        if ($lineBrush) { $lineBrush.Dispose() }
        if ($lineFont) { $lineFont.Dispose() }
    }
}

try {
    $background = [System.Drawing.Image]::FromFile($backgroundFile)
    $canvas = [System.Drawing.Bitmap]::new($width * $renderScale, $height * $renderScale)
    $graphics = [System.Drawing.Graphics]::FromImage($canvas)
    $graphics.ScaleTransform($renderScale, $renderScale)
    $graphics.InterpolationMode = [System.Drawing.Drawing2D.InterpolationMode]::HighQualityBicubic
    $graphics.CompositingQuality = [System.Drawing.Drawing2D.CompositingQuality]::HighQuality
    $graphics.PixelOffsetMode = [System.Drawing.Drawing2D.PixelOffsetMode]::HighQuality
    $graphics.TextRenderingHint = [System.Drawing.Text.TextRenderingHint]::AntiAliasGridFit

    # Cover the canvas without stretching. Focus controls the cropped-off excess.
    $coverScale = [math]::Max($width / [double]$background.Width, $height / [double]$background.Height)
    $cropWidth = $width / $coverScale
    $cropHeight = $height / $coverScale
    $cropX = ($background.Width - $cropWidth) * [double]$config.focus.x
    $cropY = ($background.Height - $cropHeight) * [double]$config.focus.y
    $graphics.DrawImage($background, [System.Drawing.RectangleF]::new(0, 0, $width, $height),
        [System.Drawing.RectangleF]::new($cropX, $cropY, $cropWidth, $cropHeight), [System.Drawing.GraphicsUnit]::Pixel)

    $versionText = "v$Version"
    if (-not [string]::IsNullOrWhiteSpace([string]$config.codename)) {
        $versionText += " [$($config.codename)]"
    }
    $lines = @(
        (Draw-BannerLine -Text $config.category -Y 43 -FontSize 42 -MinimumFontSize 36 -Style Regular -Color $config.colors.category)
        (Draw-BannerLine -Text $versionText -Y 126 -FontSize 82 -MinimumFontSize 64 -Style Bold -Color $config.colors.version)
        (Draw-BannerLine -Text $config.headline -Y 237 -FontSize 48 -MinimumFontSize 40 -Style Regular -Color $config.colors.headline)
    )
    $ruleBrush = [System.Drawing.SolidBrush]::new([System.Drawing.ColorTranslator]::FromHtml($config.colors.rule))
    $graphics.FillRectangle($ruleBrush, 146, 108, 56, 4)

    $result = [System.Drawing.Bitmap]::new($width, $height)
    $resultGraphics = [System.Drawing.Graphics]::FromImage($result)
    $resultGraphics.InterpolationMode = [System.Drawing.Drawing2D.InterpolationMode]::HighQualityBicubic
    $resultGraphics.PixelOffsetMode = [System.Drawing.Drawing2D.PixelOffsetMode]::HighQuality
    $resultGraphics.DrawImage($canvas, [System.Drawing.Rectangle]::new(0, 0, $width, $height))
    [System.IO.Directory]::CreateDirectory([System.IO.Path]::GetDirectoryName($outputFile)) | Out-Null
    $result.Save($outputFile, [System.Drawing.Imaging.ImageFormat]::Png)

    $manifest = [ordered]@{
        version = $Version
        reference = "https://github.com/White-NX/carbonPaper/releases/tag/v0.8.5"
        width = $width
        height = $height
        config = $configFile
        background = $backgroundFile
        background_sha256 = (Get-FileHash -LiteralPath $backgroundFile -Algorithm SHA256).Hash.ToLowerInvariant()
        output = $outputFile
        output_sha256 = (Get-FileHash -LiteralPath $outputFile -Algorithm SHA256).Hash.ToLowerInvariant()
        font = $fontFamily.Name
        crop = [ordered]@{ x = $cropX; y = $cropY; width = $cropWidth; height = $cropHeight }
        lines = $lines
    }
    $manifestFile = [System.IO.Path]::ChangeExtension($outputFile, ".render.json")
    [System.IO.File]::WriteAllText($manifestFile, ($manifest | ConvertTo-Json -Depth 6) + [Environment]::NewLine,
        [System.Text.UTF8Encoding]::new($false))
    Write-Host "Banner: $outputFile ($width x $height)"
    Write-Host "Render record: $manifestFile"
} finally {
    if ($ruleBrush) { $ruleBrush.Dispose() }
    if ($resultGraphics) { $resultGraphics.Dispose() }
    if ($result) { $result.Dispose() }
    if ($graphics) { $graphics.Dispose() }
    if ($canvas) { $canvas.Dispose() }
    if ($background) { $background.Dispose() }
    $format.Dispose()
    $fontFamily.Dispose()
}
