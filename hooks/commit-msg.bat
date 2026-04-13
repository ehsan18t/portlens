@echo off
REM PortLens - Commit message validation hook (Windows batch version)
REM Enforces Conventional Commits format.
REM
REM Install: copy to .git\hooks\commit-msg (remove .bat extension)

setlocal enabledelayedexpansion

set "commit_msg_file=%~1"

REM Read first line of commit message
set /p subject=<"%commit_msg_file%"

REM Skip merge commits
echo !subject! | findstr /r /c:"^Merge " >nul 2>nul
if %ERRORLEVEL% equ 0 exit /b 0

REM Skip git-generated revert commits
echo !subject! | findstr /r /c:"^Revert " >nul 2>nul
if %ERRORLEVEL% equ 0 exit /b 0

REM Use PowerShell for regex validation (batch regex is too limited)
powershell -NoProfile -Command ^
    "$s = '%subject%'; " ^
    "$pattern = '^(feat|fix|docs|style|refactor|perf|test|build|ci|chore|revert|enforce)(\([a-z0-9-]+\))?: [a-z]'; " ^
    "if ($s -notmatch $pattern) { exit 1 } " ^
    "$desc = $s -replace '^(feat|fix|docs|style|refactor|perf|test|build|ci|chore|revert|enforce)(\([a-z0-9-]+\))?: ', ''; " ^
    "if ($desc.Length -lt 5) { exit 2 } " ^
    "if ($desc.Length -gt 200) { exit 3 } " ^
    "if ($s -match '\.$') { exit 4 } " ^
    "exit 0"

set "result=%ERRORLEVEL%"

if %result% equ 1 (
    echo.
    echo X COMMIT MESSAGE REJECTED
    echo.
    echo   Your message:  "!subject!"
    echo.
    echo   Expected format: ^<type^>(^<scope^>): ^<description^>
    echo.
    echo   Allowed types:
    echo     feat, fix, docs, style, refactor, perf,
    echo     test, build, ci, chore, revert, enforce
    echo.
    echo   Rules:
    echo     - Description must start with a lowercase letter
    echo     - No period at the end
    echo     - Scope is optional: feat^(cleaner^): ...
    echo.
    echo   Examples:
    echo     feat: add memory page combining support
    echo     fix^(ntapi^): handle buffer size mismatch
    echo.
    exit /b 1
)

if %result% equ 2 (
    echo.
    echo X COMMIT MESSAGE TOO SHORT
    echo   Description must be at least 5 characters.
    echo.
    exit /b 1
)

if %result% equ 3 (
    echo.
    echo X COMMIT MESSAGE TOO LONG
    echo   Subject description must be 200 characters or fewer.
    echo.
    exit /b 1
)

if %result% equ 4 (
    echo.
    echo X COMMIT MESSAGE ENDS WITH PERIOD
    echo   Do not end the subject line with a period.
    echo.
    exit /b 1
)

exit /b 0
