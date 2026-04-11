@echo off
setlocal EnableExtensions EnableDelayedExpansion

echo.
echo ============================================
echo USB Disk Nuclear Attempt
echo ============================================
echo.
echo This script will try multiple destructive repair paths
echo against one selected disk using PowerShell and DiskPart.
echo.
echo WARNING:
echo - Do NOT select your main Windows drive.
echo - This will permanently erase the selected disk.
echo - Run this script as Administrator.
echo - If the USB controller is failing, this will still fail.
echo.

net session >nul 2>&1
if not "%errorlevel%"=="0" (
  echo This script must be run as Administrator.
  pause
  exit /b 1
)

echo Current disks:
echo.
echo list disk | diskpart
echo.
echo --------------------------------------------
echo.
set /p DISKNUM=Enter the disk number to target: 
if "%DISKNUM%"=="" (
  echo No disk number entered. Exiting.
  pause
  exit /b 1
)

echo.
echo You entered disk %DISKNUM%.
set /p CONFIRM=Type NUCLEAR to continue with disk %DISKNUM%: 
if /I not "%CONFIRM%"=="NUCLEAR" (
  echo Confirmation did not match. Exiting.
  pause
  exit /b 1
)

echo.
echo Step 1: Snapshot before changes
echo.
powershell -NoProfile -ExecutionPolicy Bypass -Command ^
  "Get-Disk -Number %DISKNUM% | Format-List Number,FriendlyName,BusType,PartitionStyle,IsReadOnly,IsOffline,OperationalStatus,HealthStatus,Size; " ^
  "Get-Partition -DiskNumber %DISKNUM% -ErrorAction SilentlyContinue | Sort-Object PartitionNumber | Format-Table DiskNumber,PartitionNumber,DriveLetter,Type,Size -Auto"

echo.
echo Step 2: Clear readonly and try to remove partitions with PowerShell
echo.
powershell -NoProfile -ExecutionPolicy Bypass -Command ^
  "$ErrorActionPreference='Continue';" ^
  "Set-Disk -Number %DISKNUM% -IsReadOnly $false -ErrorAction SilentlyContinue;" ^
  "Set-Disk -Number %DISKNUM% -IsOffline $false -ErrorAction SilentlyContinue;" ^
  "$parts = Get-Partition -DiskNumber %DISKNUM% -ErrorAction SilentlyContinue | Sort-Object PartitionNumber -Descending;" ^
  "foreach ($p in $parts) { try { Remove-Partition -DiskNumber %DISKNUM% -PartitionNumber $p.PartitionNumber -Confirm:$false -ErrorAction Stop; Write-Host ('Removed partition ' + $p.PartitionNumber + ' via PowerShell'); } catch { Write-Host ('PowerShell failed on partition ' + $p.PartitionNumber + ': ' + $_.Exception.Message); } }"

echo.
echo Step 3: Force-delete partitions with DiskPart override
echo.
set "DPSCRIPT=%temp%\diskpart-nuclear-%random%.txt"
(
  echo select disk %DISKNUM%
  echo attributes disk clear readonly
  echo select partition 2
  echo delete partition override
  echo select partition 1
  echo delete partition override
  echo attributes disk clear readonly
  echo clean
) > "%DPSCRIPT%"

diskpart /s "%DPSCRIPT%"
set "DPERROR=%errorlevel%"
del "%DPSCRIPT%" >nul 2>&1

echo.
echo Step 4: Try Clear-Disk directly
echo.
powershell -NoProfile -ExecutionPolicy Bypass -Command ^
  "$ErrorActionPreference='Continue';" ^
  "Set-Disk -Number %DISKNUM% -IsReadOnly $false -ErrorAction SilentlyContinue;" ^
  "Set-Disk -Number %DISKNUM% -IsOffline $false -ErrorAction SilentlyContinue;" ^
  "try { Clear-Disk -Number %DISKNUM% -RemoveData -RemoveOEM -Confirm:$false -ErrorAction Stop; Write-Host 'Clear-Disk succeeded'; } catch { Write-Host ('Clear-Disk failed: ' + $_.Exception.Message) }"

echo.
echo Step 5: If disk is RAW or empty, try rebuilding one exFAT partition
echo.
powershell -NoProfile -ExecutionPolicy Bypass -Command ^
  "$ErrorActionPreference='Continue';" ^
  "Set-Disk -Number %DISKNUM% -IsReadOnly $false -ErrorAction SilentlyContinue;" ^
  "Set-Disk -Number %DISKNUM% -IsOffline $false -ErrorAction SilentlyContinue;" ^
  "$disk = Get-Disk -Number %DISKNUM% -ErrorAction SilentlyContinue;" ^
  "if ($disk.PartitionStyle -eq 'RAW') { try { Initialize-Disk -Number %DISKNUM% -PartitionStyle MBR -ErrorAction Stop | Out-Null; Write-Host 'Initialize-Disk succeeded'; } catch { Write-Host ('Initialize-Disk failed: ' + $_.Exception.Message) } };" ^
  "try { $part = New-Partition -DiskNumber %DISKNUM% -UseMaximumSize -AssignDriveLetter -ErrorAction Stop; Write-Host ('New-Partition succeeded with drive letter ' + $part.DriveLetter); Format-Volume -Partition $part -FileSystem exFAT -NewFileSystemLabel 'USB' -Confirm:$false -ErrorAction Stop | Out-Null; Write-Host 'Format-Volume succeeded'; } catch { Write-Host ('Rebuild failed: ' + $_.Exception.Message) }"

echo.
echo Step 6: Snapshot after changes
echo.
powershell -NoProfile -ExecutionPolicy Bypass -Command ^
  "Get-Disk -Number %DISKNUM% | Format-List Number,FriendlyName,BusType,PartitionStyle,IsReadOnly,IsOffline,OperationalStatus,HealthStatus,Size; " ^
  "Get-Partition -DiskNumber %DISKNUM% -ErrorAction SilentlyContinue | Sort-Object PartitionNumber | Format-Table DiskNumber,PartitionNumber,DriveLetter,Type,Size -Auto; " ^
  "Get-Volume | Where-Object { $_.DriveType -eq 'Removable' } | Sort-Object DriveLetter | Format-Table DriveLetter,FileSystemLabel,FileSystem,HealthStatus,OperationalStatus,SizeRemaining,Size -Auto"

echo.
echo Finished.
echo If the disk is still read-only or the same partitions remain, the USB controller is almost certainly bad.
echo.
pause
exit /b 0
