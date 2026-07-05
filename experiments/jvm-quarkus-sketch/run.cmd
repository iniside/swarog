@echo off
setlocal enabledelayedexpansion

rem --- Build and run the Quarkus sketch, then print the admin link. ---
rem Finds a JDK 26 automatically: prefers JAVA_HOME, else the one IntelliJ
rem downloaded to %USERPROFILE%\.jdks (e.g. temurin-26.x).

if defined JAVA_HOME if exist "%JAVA_HOME%\bin\java.exe" goto have_jdk

for /d %%D in ("%USERPROFILE%\.jdks\*26*") do (
    if exist "%%D\bin\java.exe" set "JAVA_HOME=%%D"
)

if not defined JAVA_HOME (
    echo [!] No JDK 26 found. Set JAVA_HOME, or install one ^(winget install EclipseAdoptium.Temurin.26.JDK^).
    exit /b 1
)

:have_jdk
echo Using JDK: %JAVA_HOME%
echo.
echo ============================================================
echo   Admin panel:  http://localhost:8090/admin
echo   (compiling... the page is live once you see "admin on ..." below)
echo   Ctrl+C to stop.
echo ============================================================
echo.

cd /d "%~dp0"
call gradlew.bat quarkusDev
