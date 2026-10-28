@echo off
rem Puts the "Fission Export" script where Fusion 360 looks for scripts.
rem In Fusion: UTILITIES ^> ADD-INS ^> Scripts and Add-Ins ^> Fission Export ^> Run.
set "DEST=%APPDATA%\Autodesk\Autodesk Fusion 360\API\Scripts\Fission Export"
if not exist "%DEST%" mkdir "%DEST%"
xcopy /y /q "%~dp0fusion-addin\Fission Export\*.*" "%DEST%\" >nul
if errorlevel 1 (echo Could not copy the script. & pause & exit /b 1)
echo Done. In Fusion 360: UTILITIES ^> ADD-INS ^> Scripts and Add-Ins ^> Scripts ^> Fission Export ^> Run.
echo It saves a .ffx file. Open it in Fission with MANAGE ^> ADD-INS ^> Open from Fusion.
pause
