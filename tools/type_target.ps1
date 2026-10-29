# 测试用的打字目标窗口：一个置顶的文本框，接住蒜鸟打过来的字，Seconds 秒后把内容写到 Out 并关闭。
# 每次文字变化都在 Log 里记一行（时间 + 当前全文），用来看边说边打的节奏。
# 用法：powershell -STA -File type_target.ps1 -Seconds 45 -Out result.txt -Log changes.log
param([int]$Seconds = 45, [string]$Out = "type_result.txt", [string]$Log = "type_changes.log")

Add-Type -AssemblyName System.Windows.Forms
Add-Type -AssemblyName System.Drawing

$form = New-Object Windows.Forms.Form
$form.Text = "蒜鸟语音测试（$Seconds 秒后自动关闭）"
$form.Width = 1000
$form.Height = 420
$form.TopMost = $true
$form.StartPosition = "CenterScreen"

$box = New-Object Windows.Forms.TextBox
$box.Multiline = $true
$box.Dock = "Fill"
$box.Font = New-Object Drawing.Font("Microsoft YaHei", 14)
$form.Controls.Add($box)

Set-Content -Path $Log -Value "" -Encoding UTF8
$box.Add_TextChanged({ Add-Content -Path $Log -Value ("{0} {1}" -f (Get-Date -Format "HH:mm:ss.fff"), $box.Text) -Encoding UTF8 })

$timer = New-Object Windows.Forms.Timer
$timer.Interval = $Seconds * 1000
$timer.Add_Tick({
    $timer.Stop()
    [IO.File]::WriteAllText($Out, $box.Text, [Text.Encoding]::UTF8)
    $form.Close()
})
$form.Add_Shown({
    $form.Activate()
    $box.Focus()
    $timer.Start()
})
[void]$form.ShowDialog()
