@echo off
REM portview — Pre-commit hook (Windows batch version)
REM Prevents committing code that doesn't pass quality gates.
REM
REM Install: copy this file to .git\hooks\pre-commit
REM          (remove the .bat extension when copying)

echo ======================================
echo   portview Pre-Commit Quality Gate
echo ======================================
echo.

REM Gate 1: Formatting
echo -^> Checking formatting...
cargo fmt --all -- --check
if %ERRORLEVEL% neq 0 (
    echo.
    echo X FORMATTING FAILED
    echo   Run: cargo fmt
    echo   Then try committing again.
    exit /b 1
)
echo   OK Formatting

REM Gate 2: Clippy
echo -^> Running clippy...
cargo clippy --all-targets
if %ERRORLEVEL% neq 0 (
    echo.
    echo X CLIPPY FAILED
    echo   Fix the lint errors above, then try committing again.
    exit /b 1
)
echo   OK Clippy

REM Gate 3: Tests
echo -^> Running tests...
cargo test --all-targets
if %ERRORLEVEL% neq 0 (
    echo.
    echo X TESTS FAILED
    echo   Fix the failing tests, then try committing again.
    exit /b 1
)
echo   OK Tests

echo.
echo All quality gates passed. Committing...
