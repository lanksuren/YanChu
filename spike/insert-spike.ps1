# YanChu 落字链路 spike —— 在真实前台窗口上验证「修饰键守卫 / 剪贴板条件恢复 / 四态结果 / 撤销」
#
# 这个脚本不是产品代码，是产品里 InsertEngine 的行为规范 + 可执行验证。
# 里面的每个函数对应《产品定义-v2》第六节「落点与撤销」的一条约束，
# Rust 实现按同样的状态机照抄即可。
#
# 用法： powershell -ExecutionPolicy Bypass -File spike\insert-spike.ps1

$ErrorActionPreference = 'Stop'
[Console]::OutputEncoding = [Text.Encoding]::UTF8

$root = Split-Path -Parent $MyInvocation.MyCommand.Path
$dump = Join-Path $root '.target-dump.txt'
$targetExe = Join-Path $root '.target-app.exe'

# ---------------------------------------------------------------- Win32

$nativeCs = @'
using System;
using System.Runtime.InteropServices;

public static class Native
{
    [DllImport("user32.dll")] public static extern IntPtr GetForegroundWindow();
    [DllImport("user32.dll")] public static extern bool SetForegroundWindow(IntPtr hWnd);
    [DllImport("user32.dll")] public static extern bool BringWindowToTop(IntPtr hWnd);
    [DllImport("user32.dll")] public static extern bool IsWindow(IntPtr hWnd);
    [DllImport("user32.dll")] public static extern short GetAsyncKeyState(int vKey);
    [DllImport("user32.dll")] public static extern uint GetWindowThreadProcessId(IntPtr hWnd, out uint pid);
    [DllImport("user32.dll")] public static extern bool AttachThreadInput(uint attach, uint attachTo, bool flag);
    [DllImport("user32.dll")] public static extern bool ShowWindow(IntPtr hWnd, int cmd);
    [DllImport("user32.dll")] public static extern int GetWindowTextLength(IntPtr hWnd);
    [DllImport("user32.dll", CharSet = CharSet.Unicode)] public static extern int GetWindowText(IntPtr hWnd, System.Text.StringBuilder sb, int max);
    [DllImport("user32.dll")] public static extern void keybd_event(byte vk, byte scan, uint flags, UIntPtr extra);

    public static void KeyDown(byte vk) { keybd_event(vk, 0, 0x0000, UIntPtr.Zero); }
    public static void KeyUp(byte vk)   { keybd_event(vk, 0, 0x0002, UIntPtr.Zero); }

    public static uint ThreadOf(IntPtr hWnd) { uint p; return GetWindowThreadProcessId(hWnd, out p); }

    public static string Title(IntPtr hWnd)
    {
        int n = GetWindowTextLength(hWnd);
        var sb = new System.Text.StringBuilder(n + 2);
        GetWindowText(hWnd, sb, sb.Capacity);
        return sb.ToString();
    }

    // 后台进程调用 SetForegroundWindow 会被前台锁挡住，必须先 AttachThreadInput 到当前前台线程
    public static bool ForceForeground(IntPtr hWnd)
    {
        IntPtr fg = GetForegroundWindow();
        if (fg == hWnd) return true;

        uint fgThread = ThreadOf(fg);
        uint targetThread = ThreadOf(hWnd);
        bool attached = false;
        if (fgThread != targetThread && fgThread != 0)
            attached = AttachThreadInput(fgThread, targetThread, true);
        try
        {
            ShowWindow(hWnd, 9); // SW_RESTORE
            BringWindowToTop(hWnd);
            SetForegroundWindow(hWnd);
        }
        finally
        {
            if (attached) AttachThreadInput(fgThread, targetThread, false);
        }
        return GetForegroundWindow() == hWnd;
    }

    // 用于模拟「真人手慢，松手 200ms 后修饰键才物理抬起」
    public static void ReleaseAfter(byte vk, int ms)
    {
        var t = new System.Threading.Thread(delegate ()
        {
            System.Threading.Thread.Sleep(ms);
            keybd_event(vk, 0, 0x0002, UIntPtr.Zero);
        });
        t.IsBackground = true;
        t.Start();
    }
}
'@

Add-Type -TypeDefinition $nativeCs -Language CSharp

$VK_SHIFT = 0x10; $VK_CONTROL = 0x11; $VK_MENU = 0x12; $VK_LWIN = 0x5B; $VK_RWIN = 0x5C
$VK_LSHIFT = 0xA0; $VK_V = 0x56; $VK_BACK = 0x08; $VK_A = 0x41
$MODIFIERS = @($VK_SHIFT, $VK_CONTROL, $VK_MENU, $VK_LWIN, $VK_RWIN)

# ---------------------------------------------------------------- 产品逻辑（规范）

# 约束一：修饰键守卫。按下热键时若 Ctrl/Alt/Shift/Win 仍按着，
# 注入的按键会被合成（Ctrl+V 变成 Ctrl+Ctrl+V 之类），必须等它们抬起。
function Wait-ModifiersReleased {
    param([int]$TimeoutMs = 500, [int]$PollMs = 10)
    $sw = [Diagnostics.Stopwatch]::StartNew()
    while ($sw.ElapsedMilliseconds -lt $TimeoutMs) {
        $held = $false
        foreach ($vk in $MODIFIERS) {
            if (([Native]::GetAsyncKeyState($vk) -band 0x8000) -ne 0) { $held = $true; break }
        }
        if (-not $held) { return [pscustomobject]@{ Ok = $true; WaitedMs = $sw.ElapsedMilliseconds } }
        Start-Sleep -Milliseconds $PollMs
    }
    $stillHeld = $false
    foreach ($vk in $MODIFIERS) {
        if (([Native]::GetAsyncKeyState($vk) -band 0x8000) -ne 0) { $stillHeld = $true; break }
    }
    return [pscustomobject]@{ Ok = (-not $stillHeld); WaitedMs = $sw.ElapsedMilliseconds }
}

# 落点：按下热键的瞬间记下来，之后不再重新取（否则焦点一变就写错地方）
function Get-InsertionAnchor {
    $hwnd = [Native]::GetForegroundWindow()
    if ($hwnd -eq [IntPtr]::Zero) { return [pscustomobject]@{ Ok = $false; Hwnd = $null; ProcId = 0 } }
    $ownerPid = 0
    [void][Native]::GetWindowThreadProcessId($hwnd, [ref]$ownerPid)
    return [pscustomobject]@{ Ok = $true; Hwnd = $hwnd; ProcId = $ownerPid }
}

function Get-ClipboardText {
    for ($i = 0; $i -lt 6; $i++) {
        try {
            $t = Get-Clipboard -Raw -ErrorAction Stop
            if ($null -eq $t) { return '' }
            return $t.TrimEnd("`r", "`n")
        } catch { Start-Sleep -Milliseconds 25 }
    }
    return ''
}

# 约束二：剪贴板条件恢复。只有当剪贴板里还是我们写进去的内容时才还原，
# 否则说明用户中途复制了别的东西，还原就等于毁掉他的剪贴板。
function Test-ClipboardRestore {
    param([string]$InsertedText)
    $current = Get-ClipboardText
    if ($current -ceq $InsertedText) { return 'Restore' }
    return 'Keep'
}

function Send-CtrlV {
    [Native]::KeyDown($VK_CONTROL)
    Start-Sleep -Milliseconds 15
    [Native]::KeyDown($VK_V)
    Start-Sleep -Milliseconds 15
    [Native]::KeyUp($VK_V)
    Start-Sleep -Milliseconds 15
    [Native]::KeyUp($VK_CONTROL)
}

# 约束三：写入结果四态。不假设粘贴一定成功，写完读回来比对。
function Invoke-ClipboardInsert {
    param([string]$Text)

    $snapshot = Get-ClipboardText
    $before = Get-TargetText

    try {
        Set-Clipboard -Value $Text -ErrorAction Stop
    } catch {
        return [pscustomobject]@{ Status = 'Failed'; CharsInserted = 0; Warning = 'clipboard-open-failed'; Message = $_.Exception.Message }
    }

    Send-CtrlV

    # 目标窗口的 dump 有 150ms 刷新间隔，这里轮询而不是睡死
    $sw = [Diagnostics.Stopwatch]::StartNew()
    $after = $before
    $matched = $false
    while ($sw.ElapsedMilliseconds -lt 900) {
        Start-Sleep -Milliseconds 60
        $after = Get-TargetText
        if ($after.EndsWith($Text) -and $after.Length -ge $before.Length) { $matched = $true; break }
    }

    if (-not $matched) {
        return [pscustomobject]@{ Status = 'CopiedFallback'; CharsInserted = 0; Warning = 'paste-not-taken'; Message = "目标未收到文本（长度 $($before.Length) → $($after.Length)）" }
    }

    if ((Test-ClipboardRestore -InsertedText $Text) -eq 'Restore' -and $snapshot.Length -gt 0) {
        Set-Clipboard -Value $snapshot
    }

    return [pscustomobject]@{ Status = 'Inserted'; CharsInserted = $Text.Length; Warning = $null; Message = $null }
}

# ---------------------------------------------------------------- 测试脚手架

function Get-TargetText {
    if (Test-Path $dump) { try { return [IO.File]::ReadAllText($dump, [Text.Encoding]::UTF8) } catch { return '' } }
    return ''
}

function Wait-TargetText {
    param([string]$Expected, [int]$TimeoutMs = 2500)
    $sw = [Diagnostics.Stopwatch]::StartNew()
    while ($sw.ElapsedMilliseconds -lt $TimeoutMs) {
        if ((Get-TargetText) -ceq $Expected) { return $true }
        Start-Sleep -Milliseconds 50
    }
    return $false
}

function Focus-Target {
    param([IntPtr]$Hwnd, [int]$ProcId)
    for ($i = 0; $i -lt 25; $i++) {
        if ([Native]::GetForegroundWindow() -eq $Hwnd) { return $true }
        if ($i -eq 3) {
            # 先试 COM 的 AppActivate，它走的是另一条允许抢焦点的路径
            try {
                $sh = New-Object -ComObject WScript.Shell
                [void]$sh.AppActivate($ProcId)
            } catch { }
        }
        [void][Native]::ForceForeground($Hwnd)
        Start-Sleep -Milliseconds 60
    }
    return ([Native]::GetForegroundWindow() -eq $Hwnd)
}

function Clear-Target {
    if ((Get-TargetText).Length -gt 0) {
        [Native]::KeyDown($VK_CONTROL); [Native]::KeyDown($VK_A); [Native]::KeyUp($VK_A); [Native]::KeyUp($VK_CONTROL)
        Start-Sleep -Milliseconds 60
        [Native]::KeyDown($VK_BACK); [Native]::KeyUp($VK_BACK)
        Start-Sleep -Milliseconds 200
    }
    [void](Wait-TargetText -Expected '')
}

$results = New-Object System.Collections.ArrayList
function Add-Result {
    param([string]$Name, [string]$Expect, [string]$Actual, [bool]$Pass)
    [void]$results.Add([pscustomobject]@{ 用例 = $Name; 结果 = $(if ($Pass) { 'PASS' } else { 'FAIL' }) })
    Write-Host ("  [{0}] {1}" -f $(if ($Pass) { 'PASS' } else { 'FAIL' }), $Name)
    if (-not $Pass) { Write-Host ("        期望: {0}" -f $Expect); Write-Host ("        实际: {0}" -f $Actual) }
}

# ---------------------------------------------------------------- 起目标窗口

if (-not (Test-Path $targetExe) -or (Get-Item (Join-Path $root 'target-app.cs')).LastWriteTime -gt (Get-Item $targetExe).LastWriteTime) {
    Write-Host '编译假目标窗口...'
    # 必须显式按 UTF-8 读：Windows PowerShell 5.1 把无 BOM 的文件当 GBK 解，
    # 中文注释末尾的字节会吃掉换行，把后面的 using 也变成注释的一部分。
    $targetCs = [IO.File]::ReadAllText((Join-Path $root 'target-app.cs'), [Text.Encoding]::UTF8)
    # 注意：一旦显式传 -ReferencedAssemblies，PowerShell 就不再注入默认引用，
    # 必须把 System / System.Core 也写上，否则连 [STAThread] 都找不到。
    Add-Type -TypeDefinition $targetCs -OutputAssembly $targetExe -OutputType WindowsApplication `
        -ReferencedAssemblies 'System', 'System.Core', 'System.Windows.Forms', 'System.Drawing'
}

Remove-Item $dump -ErrorAction SilentlyContinue
$targetProc = Start-Process -FilePath $targetExe -ArgumentList $dump -PassThru
Start-Sleep -Milliseconds 1200
$hwnd = [IntPtr]::Zero
for ($i = 0; $i -lt 40; $i++) {
    $targetProc.Refresh()
    if ($targetProc.MainWindowHandle -ne [IntPtr]::Zero) { $hwnd = $targetProc.MainWindowHandle; break }
    Start-Sleep -Milliseconds 100
}
if ($hwnd -eq [IntPtr]::Zero) { throw '目标窗口没起来' }

if (-not (Focus-Target -Hwnd $hwnd -ProcId $targetProc.Id)) {
    $fg = [Native]::GetForegroundWindow()
    throw ("抢不到焦点。目标={0}「{1}」，当前前台={2}「{3}」" -f $hwnd, ([Native]::Title($hwnd)), $fg, ([Native]::Title($fg)))
}

Write-Host ''
Write-Host '=== 落字链路 spike ===' -ForegroundColor Cyan
Write-Host ("目标窗口 hwnd={0} pid={1}「{2}」" -f $hwnd, $targetProc.Id, [Native]::Title($hwnd))
Write-Host ''

try {
    # ---------- 用例 1：基础粘贴 + 剪贴板恢复
    Clear-Target
    Set-Clipboard -Value 'PREV-CLIP'
    $text1 = '你好，世界 hello world'
    $r1 = Invoke-ClipboardInsert -Text $text1
    $ok1 = (Wait-TargetText -Expected $text1)
    $clip1 = Get-ClipboardText
    Add-Result '01 基础粘贴 + 剪贴板条件恢复' 'Inserted，目标文本一致，剪贴板还原成 PREV-CLIP' `
        ("{0}，文本={1}，剪贴板={2}" -f $r1.Status, $(if ($ok1) { '一致' } else { '不一致' }), $clip1) `
        ($ok1 -and $r1.Status -eq 'Inserted' -and $clip1 -ceq 'PREV-CLIP')

    # ---------- 用例 2：修饰键一直按住 —— 守卫必须在 500ms 后放弃，且不写
    Clear-Target
    $before2 = Get-TargetText
    [Native]::KeyDown($VK_LSHIFT)
    Start-Sleep -Milliseconds 60
    $g2 = Wait-ModifiersReleased -TimeoutMs 500
    [Native]::KeyUp($VK_LSHIFT)
    Start-Sleep -Milliseconds 120
    Add-Result '02 修饰键一直按住 → 守卫超时放弃' 'Ok=false，等待 ≈500ms，目标文本不变' `
        ("Ok={0}，等待={1}ms，文本不变={2}" -f $g2.Ok, $g2.WaitedMs, ((Get-TargetText) -ceq $before2)) `
        ((-not $g2.Ok) -and $g2.WaitedMs -ge 480 -and (Get-TargetText) -ceq $before2)

    # ---------- 用例 3：修饰键 200ms 后抬起 —— 守卫应等到它抬起再放行
    Clear-Target
    [Native]::KeyDown($VK_LSHIFT)
    Start-Sleep -Milliseconds 40
    [Native]::ReleaseAfter($VK_LSHIFT, 200)
    $g3 = Wait-ModifiersReleased -TimeoutMs 500
    $r3 = $null
    if ($g3.Ok) { $r3 = Invoke-ClipboardInsert -Text '守卫放行后写入' }
    $ok3 = (Wait-TargetText -Expected '守卫放行后写入')
    Add-Result '03 修饰键 200ms 后抬起 → 守卫等待放行' 'Ok=true，等待 150~350ms，写入成功' `
        ("Ok={0}，等待={1}ms，写入={2}" -f $g3.Ok, $g3.WaitedMs, $(if ($ok3) { '成功' } else { '失败' })) `
        ($g3.Ok -and $g3.WaitedMs -ge 120 -and $g3.WaitedMs -le 400 -and $ok3)

    # ---------- 用例 4：用户中途改了剪贴板 —— 不能还原，否则毁掉他的内容
    Clear-Target
    Set-Clipboard -Value 'USER-ORIGINAL'
    [void](Invoke-ClipboardInsert -Text '插入后我复制了别的东西')
    Set-Clipboard -Value 'USER-REPLACED'
    Start-Sleep -Milliseconds 80
    $decision = Test-ClipboardRestore -InsertedText '插入后我复制了别的东西'
    $clip4 = Get-ClipboardText
    Add-Result '04 剪贴板被用户改写 → 不还原' '决策=Keep，剪贴板保持 USER-REPLACED' `
        ("决策={0}，剪贴板={1}" -f $decision, $clip4) `
        ($decision -eq 'Keep' -and $clip4 -ceq 'USER-REPLACED')

    # ---------- 用例 5：落点在按下瞬间锁定
    Clear-Target
    $anchor = Get-InsertionAnchor
    Start-Sleep -Milliseconds 50
    $anchorValid = [Native]::IsWindow($anchor.Hwnd)
    Add-Result '05 落点在按下时锁定（不重新取焦点）' 'anchor 记录的窗口 = 当前前台，且窗口有效' `
        ("anchor==当前前台:{0}，IsWindow:{1}" -f ($anchor.Hwnd -eq [Native]::GetForegroundWindow()), $anchorValid) `
        ($anchor.Ok -and $anchorValid -and $anchor.Hwnd -eq [Native]::GetForegroundWindow())

} finally {
    Write-Host ''
    $passCount = @($results | Where-Object { $_.结果 -eq 'PASS' }).Count
    $color = if ($passCount -eq $results.Count) { 'Green' } else { 'Red' }
    Write-Host ("合计 {0}/{1} 通过" -f $passCount, $results.Count) -ForegroundColor $color
    Stop-Process -Id $targetProc.Id -Force -ErrorAction SilentlyContinue
    Remove-Item $dump -ErrorAction SilentlyContinue
}
