Add-Type -TypeDefinition @'
using System;
using System.Runtime.InteropServices;

public static class BrowsCloseProbe {
    [DllImport("user32.dll", ExactSpelling = true, SetLastError = true)]
    [return: MarshalAs(UnmanagedType.Bool)]
    public static extern bool PostMessageW(IntPtr window, uint message, IntPtr wParam, IntPtr lParam);

    [DllImport("user32.dll", ExactSpelling = true)]
    public static extern uint GetWindowThreadProcessId(IntPtr window, out uint processId);
}
'@

function Start-BrowsProbe([string]$Executable, [string]$TracePath) {
    $previousTrace = $env:BROWS_LIFECYCLE_TRACE
    try {
        $env:BROWS_LIFECYCLE_TRACE = '1'
        Start-Process -FilePath $Executable -RedirectStandardError $TracePath -PassThru
    } finally {
        $env:BROWS_LIFECYCLE_TRACE = $previousTrace
    }
}

function Request-BrowsClose([Diagnostics.Process]$Process) {
    $Process.Refresh()
    $window = $Process.MainWindowHandle
    $owner = [uint32]0
    if ($window -eq [IntPtr]::Zero -or $Process.MainWindowTitle -ne 'Brows' -or
        [BrowsCloseProbe]::GetWindowThreadProcessId($window, [ref]$owner) -eq 0 -or $owner -ne $Process.Id) {
        throw 'The Brows main window is no longer owned by the test process'
    }
    # Process.CloseMainWindow ignores the native PostMessage result on Windows.
    if (-not [BrowsCloseProbe]::PostMessageW($window, 0x0010, [IntPtr]::Zero, [IntPtr]::Zero)) {
        $code = [Runtime.InteropServices.Marshal]::GetLastWin32Error()
        throw "WM_CLOSE could not be posted (Win32 error $code)"
    }
}

function Read-BrowsLifecycleTrace([string]$TracePath) {
    if (Test-Path -LiteralPath $TracePath) {
        Get-Content -LiteralPath $TracePath -Tail 20 | Where-Object { $_ -cmatch '^brows-lifecycle [a-z-]+$' }
    }
}
