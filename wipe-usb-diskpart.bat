@echo off
setlocal EnableExtensions EnableDelayedExpansion

echo.
echo ============================================
echo USB Disk Wipe Helper
echo ============================================
echo.
echo This script will use DISKPART to completely wipe
echo the selected disk by removing all partitions.
echo.
echo WARNING:
echo - Do NOT select your main Windows drive.
echo - This will permanently erase the selected disk.
echo - Run this script as Administrator.
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
echo Please identify the USB disk number above.
echo Make sure you do NOT choose the system disk.
echo.
set /p DISKNUM=Enter the disk number to wipe: 

if "%DISKNUM%"=="" (
  echo No disk number entered. Exiting.
  pause
  exit /b 1
)

echo.
echo You entered disk %DISKNUM%.
set /p CONFIRM=Type WIPE to erase disk %DISKNUM%: 

if /I not "%CONFIRM%"=="WIPE" (
  echo Confirmation did not match. Exiting without changes.
  pause
  exit /b 1
)

set "DPSCRIPT=%temp%\diskpart-wipe-%random%.txt"
(
  echo select disk %DISKNUM%
  echo detail disk
  echo attributes disk clear readonly
  echo clean
) > "%DPSCRIPT%"

echo.
echo Running DiskPart against disk %DISKNUM%...
echo.
diskpart /s "%DPSCRIPT%"
set "DPERROR=%errorlevel%"

del "%DPSCRIPT%" >nul 2>&1

echo.
if "%DPERROR%"=="0" (
  echo DiskPart finished. Review the output above to confirm the clean succeeded.
) else (
  echo DiskPart returned error code %DPERROR%.
  echo If you see an I/O device error, the USB drive may be failing.
)
echo.
pause
exit /b %DPERROR%
