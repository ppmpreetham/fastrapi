$files = Get-ChildItem -Path ".\src" -Recurse -Filter *.rs -File

$content = foreach ($file in $files) {
    Get-Content $file.FullName -Raw
}

$final = ($content -join " ") -replace "(\r\n|\n|\r|\t)", " "

Set-Clipboard -Value $final

Write-Host "done."