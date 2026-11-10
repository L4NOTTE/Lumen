# Rebuild PNG, RGBA and multi-resolution ICO from the same Lumen geometry.
Add-Type -AssemblyName System.Drawing
$assetRoot=[IO.Path]::GetFullPath((Join-Path $PSScriptRoot '..\assets'))
function Render-Lumen([int]$size) {
  $bitmap=[Drawing.Bitmap]::new($size,$size,[Drawing.Imaging.PixelFormat]::Format32bppArgb)
  $g=[Drawing.Graphics]::FromImage($bitmap)
  $g.SmoothingMode=[Drawing.Drawing2D.SmoothingMode]::AntiAlias
  $g.Clear([Drawing.Color]::Transparent)
  $g.ScaleTransform($size/256.0,$size/256.0)
  $path=[Drawing.Drawing2D.GraphicsPath]::new()
  foreach($arc in @(@(8,8,112,112,180),@(136,8,112,112,270),@(136,136,112,112,0),@(8,136,112,112,90))) { $path.AddArc([single]$arc[0],[single]$arc[1],[single]$arc[2],[single]$arc[3],[single]$arc[4],90) }
  $path.CloseFigure()
  $bg=[Drawing.Drawing2D.LinearGradientBrush]::new([Drawing.Point]::new(16,16),[Drawing.Point]::new(240,240),[Drawing.Color]::FromArgb(20,29,52),[Drawing.Color]::FromArgb(39,38,75))
  $g.FillPath($bg,$path)
  $beam=[Drawing.Drawing2D.GraphicsPath]::new()
  $beam.AddPolygon([Drawing.PointF[]]@([Drawing.PointF]::new(94,164),[Drawing.PointF]::new(200,48),[Drawing.PointF]::new(210,134)))
  $beamBrush=[Drawing.Drawing2D.LinearGradientBrush]::new([Drawing.Point]::new(100,172),[Drawing.Point]::new(206,58),[Drawing.Color]::FromArgb(20,86,220,255),[Drawing.Color]::FromArgb(150,104,125,255))
  $g.FillPath($beamBrush,$beam)
  $mark=[Drawing.Drawing2D.GraphicsPath]::new()
  $mark.AddPolygon([Drawing.PointF[]]@([Drawing.PointF]::new(68,62),[Drawing.PointF]::new(104,62),[Drawing.PointF]::new(104,150),[Drawing.PointF]::new(184,150),[Drawing.PointF]::new(184,186),[Drawing.PointF]::new(68,186)))
  $light=[Drawing.Drawing2D.LinearGradientBrush]::new([Drawing.Point]::new(68,62),[Drawing.Point]::new(184,186),[Drawing.Color]::White,[Drawing.Color]::FromArgb(76,221,245))
  $g.FillPath($light,$mark)
  $star=[Drawing.Drawing2D.GraphicsPath]::new()
  $star.AddPolygon([Drawing.PointF[]]@([Drawing.PointF]::new(181,54),[Drawing.PointF]::new(188,70),[Drawing.PointF]::new(204,77),[Drawing.PointF]::new(188,84),[Drawing.PointF]::new(181,100),[Drawing.PointF]::new(174,84),[Drawing.PointF]::new(158,77),[Drawing.PointF]::new(174,70)))
  $g.FillPath([Drawing.Brushes]::White,$star)
  foreach($o in @($star,$light,$mark,$beamBrush,$beam,$bg,$path,$g)) { $o.Dispose() }
  return $bitmap
}
$pngs=@()
foreach($size in @(16,24,32,48,64,128,256)) {
  $bmp=Render-Lumen $size
  $stream=[IO.MemoryStream]::new()
  $bmp.Save($stream,[Drawing.Imaging.ImageFormat]::Png)
  $pngs+=,[byte[]]$stream.ToArray()
  if($size -eq 256) { [IO.File]::WriteAllBytes((Join-Path $assetRoot 'lumen.png'),$stream.ToArray()) }
  if($size -eq 64) {
    $rgba=[byte[]]::new(64*64*4)
    for($y=0;$y -lt 64;$y++){ for($x=0;$x -lt 64;$x++) { $c=$bmp.GetPixel($x,$y); $i=($y*64+$x)*4; $rgba[$i]=$c.R;$rgba[$i+1]=$c.G;$rgba[$i+2]=$c.B;$rgba[$i+3]=$c.A } }
    [IO.File]::WriteAllBytes((Join-Path $assetRoot 'icon64.rgba'),$rgba)
  }
  $stream.Dispose();$bmp.Dispose()
}
$ico=[IO.MemoryStream]::new();$writer=[IO.BinaryWriter]::new($ico)
$writer.Write([uint16]0);$writer.Write([uint16]1);$writer.Write([uint16]7)
$offset=6+7*16;$sizes=@(16,24,32,48,64,128,256)
for($i=0;$i -lt 7;$i++) { $v=if($sizes[$i] -eq 256){0}else{$sizes[$i]};$writer.Write([byte]$v);$writer.Write([byte]$v);$writer.Write([uint16]0);$writer.Write([uint16]1);$writer.Write([uint16]32);$writer.Write([uint32]$pngs[$i].Length);$writer.Write([uint32]$offset);$offset+=$pngs[$i].Length }
foreach($png in $pngs) { $writer.Write([byte[]]$png) }
$writer.Flush();[IO.File]::WriteAllBytes((Join-Path $assetRoot 'lumen.ico'),$ico.ToArray());$writer.Dispose();$ico.Dispose()
$uiPath=Join-Path $PSScriptRoot '..\ui\ui.html'
$html=[IO.File]::ReadAllText($uiPath)
$src='data:image/png;base64,'+[Convert]::ToBase64String([IO.File]::ReadAllBytes((Join-Path $assetRoot 'lumen.png')))
$html=[regex]::Replace($html,'<img id="logo"[^>]*>','<img id="logo" alt="Lumen" src="'+$src+'">')
[IO.File]::WriteAllText($uiPath,$html)
