@echo off
rem Fission: checks the Bambu Studio features without printing anything.
cd /d "%~dp0"
set LOG=%USERPROFILE%\Fission\bambu-test.txt
if not exist "%USERPROFILE%\Fission" mkdir "%USERPROFILE%\Fission"
echo Fission Bambu test, %DATE% %TIME% > "%LOG%"
echo.
echo 1/3  Printers Bambu Studio is connected to (and which answer on the network)...
echo ==== bambu.printers >> "%LOG%"
solidcraft-cli.exe exec bambu.printers >> "%LOG%" 2>&1
echo 2/3  Bambu Studio presets for the first printer...
echo ==== bambu.presets >> "%LOG%"
solidcraft-cli.exe exec bambu.presets >> "%LOG%" 2>&1
echo 3/3  Slicing a 20 mm test cube with Bambu Studio (can take a minute)...
echo ==== bambu.slice >> "%LOG%"
solidcraft-cli.exe run bambu-test-slice.json --results >> "%LOG%" 2>&1
echo.
type "%LOG%"
echo.
echo Saved to %LOG%  - nothing was sent to a printer.
pause
