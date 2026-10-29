# 打印电脑当前的前台窗口标题和进程名：往电脑上打字的测试开始前先确认，别打进别的窗口
Add-Type @"
using System; using System.Runtime.InteropServices; using System.Text;
public class Fg {
    [DllImport("user32.dll")] public static extern IntPtr GetForegroundWindow();
    [DllImport("user32.dll", CharSet = CharSet.Unicode)] public static extern int GetWindowText(IntPtr h, StringBuilder s, int n);
    [DllImport("user32.dll")] public static extern uint GetWindowThreadProcessId(IntPtr h, out uint pid);
}
"@
[Console]::OutputEncoding = [Text.Encoding]::UTF8
$h = [Fg]::GetForegroundWindow()
$title = New-Object System.Text.StringBuilder 512
[void][Fg]::GetWindowText($h, $title, 512)
$procId = 0
[void][Fg]::GetWindowThreadProcessId($h, [ref]$procId)
"{0}|{1}" -f $title.ToString(), (Get-Process -Id $procId).ProcessName
