@echo off
setlocal EnableExtensions EnableDelayedExpansion

echo.
echo ============================================
echo USB Disk Last-Resort Repair Helper
echo ============================================
echo.
echo This script will aggressively try to wipe and rebuild
echo the selected disk using DiskPart and PowerShell.
echo.
echo WARNING:
echo - Do NOT select your main Windows drive.
echo - This will permanently erase the selected disk.
echo - Run this script as Administrator.
echo - If the USB hardware is failing, this may still not work.
echo.

net session >nul 2>&1
if not "%errorlevel%"=="0" (
  echo This script must be run as Administrator.
  echo Right-click it and choose "Run as administrator".
  pause
  exit /b 1
)

echo Current disks:
echo.
echo list disk | diskpart
echo.
echo --------------------------------------------
echo.
set /p DISKNUM=Enter the disk number to repair/wipe: 
if "%DISKNUM%"=="" (
  echo No disk number entered. Exiting.
  pause
  exit /b 1
)

echo.
echo You entered disk %DISKNUM%.
set /p CONFIRM=Type DESTROY to continue with disk %DISKNUM%: 
if /I not "%CONFIRM%"=="DESTROY" (
  echo Confirmation did not match. Exiting without changes.
  pause
  exit /b 1
)

set "DPSCRIPT=%temp%\diskpart-aggressive-%random%.txt"
(
  echo select disk %DISKNUM%
  echo detail disk
  echo attributes disk clear readonly
  echo offline disk
  echo online disk
  echo rescan
  echo select disk %DISKNUM%
  echo clean
) > "%DPSCRIPT%"

echo.
echo Step 1: DiskPart unlock and clean attempt...
echo.
diskpart /s "%DPSCRIPT%"
set "DPERROR=%errorlevel%"
del "%DPSCRIPT%" >nul 2>&1

if "%DPERROR%"=="0" goto rebuild

echo.
echo DiskPart clean failed with error code %DPERROR%.
echo Trying PowerShell storage cmdlets as a fallback...
echo.

powershell -NoProfile -ExecutionPolicy Bypass -Command ^
  "$ErrorActionPreference='Stop';" ^
  "Set-Disk -Number %DISKNUM% -IsReadOnly $false -ErrorAction SilentlyContinue;" ^
  "Set-Disk -Number %DISKNUM% -IsOffline $false -ErrorAction SilentlyContinue;" ^
  "Clear-Disk -Number %DISKNUM% -RemoveData -RemoveOEM -Confirm:\$false"
set "PSERROR=%errorlevel%"

if not "%PSERROR%"=="0" (
  echo.
  echo PowerShell Clear-Disk failed with error code %PSERROR%.
  echo Trying one more pass with Initialize-Disk and New-Partition...
  echo.
  powershell -NoProfile -ExecutionPolicy Bypass -Command ^
    "$ErrorActionPreference='Stop';" ^
    "Set-Disk -Number %DISKNUM% -IsReadOnly $false -ErrorAction SilentlyContinue;" ^
    "Set-Disk -Number %DISKNUM% -IsOffline $false -ErrorAction SilentlyContinue;" ^
    "$disk = Get-Disk -Number %DISKNUM%;" ^
    "if ($disk.PartitionStyle -eq 'RAW') { Initialize-Disk -Number %DISKNUM% -PartitionStyle MBR -PassThru | Out-Null };" ^
    "$part = New-Partition -DiskNumber %DISKNUM% -UseMaximumSize -AssignDriveLetter;" ^
    "Format-Volume -Partition $part -FileSystem exFAT -NewFileSystemLabel 'USB' -Confirm:$false"
  set "PSREBUILDERROR=%errorlevel%"
  if not "%PSREBUILDERROR%"=="0" (
    echo.
    echo PowerShell rebuild also failed with error code %PSREBUILDERROR%.
    echo This usually means the USB device itself cannot accept writes.
    goto verify
  )
  goto verify
)

:rebuild
echo.
echo Step 2: Rebuilding the disk as one exFAT partition...
echo.

set "DPSCRIPT=%temp%\diskpart-rebuild-%random%.txt"
(
  echo select disk %DISKNUM%
  echo convert mbr
  echo create partition primary
  echo format fs=exfat quick label=USB
  echo assign
  echo detail disk
) > "%DPSCRIPT%"

diskpart /s "%DPSCRIPT%"
set "REBUILDERROR=%errorlevel%"
del "%DPSCRIPT%" >nul 2>&1

if not "%REBUILDERROR%"=="0" (
  echo.
  echo Rebuild step failed with error code %REBUILDERROR%.
)

:verify
echo.
echo Step 3: Current disk state
echo.
powershell -NoProfile -ExecutionPolicy Bypass -Command ^
  "Get-Disk -Number %DISKNUM% | Format-List Number,FriendlyName,BusType,PartitionStyle,IsReadOnly,IsOffline,OperationalStatus,HealthStatus,Size; " ^
  "Get-Partition -DiskNumber %DISKNUM% -ErrorAction SilentlyContinue | Format-Table DiskNumber,PartitionNumber,DriveLetter,Type,Size -Auto"

echo.
echo Finished. Review the output above.
echo If wipe and rebuild both failed, the USB stick is very likely bad.
echo.
pause
exit /b 0
