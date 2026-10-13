# SQLStudio — 로컬 LLM(Ollama) 연결 진단
# 되는 PC 와 안 되는 PC 에서 각각 실행해 결과를 비교한다. 설정을 바꾸지 않고 읽기만 한다.
# 실행:  powershell -ExecutionPolicy Bypass -File llm-connection-check.ps1
# 결과는 화면과 바탕화면의 llm-check-<PC이름>.txt 에 남는다.

param([string]$Url = "")

$out = Join-Path ([Environment]::GetFolderPath("Desktop")) "llm-check-$env:COMPUTERNAME.txt"
function Section($t) { "`n===== $t =====" }
$cfgPath = Join-Path $env:APPDATA "sqlstudio\config.toml"

& {
  "SQLStudio LLM 연결 진단 — $env:COMPUTERNAME / $env:USERNAME / $(Get-Date -Format 'yyyy-MM-dd HH:mm')"

  Section "1. SQLStudio 의 AI 공급자 설정 (config.toml 의 [[llm]])"
  if (Test-Path $cfgPath) {
    $lines = Get-Content $cfgPath -Encoding UTF8
    $in = $false
    foreach ($l in $lines) {
      if ($l -match '^\[\[llm\]\]') { $in = $true; "[[llm]]"; continue }
      elseif ($l -match '^\[') { $in = $false }
      if ($in -and $l -match '^(name|kind|base_url|model|proxy)\s*=') { "  $l" }
    }
  } else { "설정 파일 없음: $cfgPath" }

  # 진단할 주소: 인자 > 설정의 첫 ollama base_url > 기본
  if (-not $Url) {
    $m = (Get-Content $cfgPath -Encoding UTF8 -ErrorAction SilentlyContinue | Select-String '^base_url\s*=\s*"([^"]+)"' | Select-Object -First 1)
    $Url = if ($m) { $m.Matches[0].Groups[1].Value } else { "http://127.0.0.1:11434" }
  }
  if ($Url -notmatch '://') { $Url = "http://$Url" }
  $Url = $Url.TrimEnd('/')
  "진단할 주소: $Url"

  Section "2. Ollama 가 듣는 주소 (포트 11434)"
  netstat -ano | Select-String ":11434\s" | ForEach-Object { $_.Line.Trim() }
  "(0.0.0.0 / 127.0.0.1 / [::1] 중 무엇인지 — 127.0.0.1 만이면 이 PC 에서만, localhost 가 ::1 로 풀리면 안 될 수 있다)"

  Section "3. ollama 에 받아 둔 모델 (설정의 model 과 이름이 정확히 같아야 한다)"
  try { ollama list 2>&1 } catch { "ollama 명령 없음" }

  Section "4. localhost 가 어느 주소로 풀리나"
  try { [System.Net.Dns]::GetHostAddresses("localhost") | ForEach-Object { $_.ToString() } } catch { $_.Exception.Message }

  Section "5. 프록시 환경변수 (SQLStudio 이전 버전은 이것이 있으면 localhost 도 프록시로 보냈다)"
  Get-ChildItem env: | Where-Object { $_.Name -match 'proxy' } | ForEach-Object { "$($_.Name)=$($_.Value)" }
  "(없으면 아무것도 안 나옴)"

  Section "6. Windows 인터넷 프록시 설정 (브라우저용)"
  $k = "HKCU:\Software\Microsoft\Windows\CurrentVersion\Internet Settings"
  $p = Get-ItemProperty $k -ErrorAction SilentlyContinue
  "ProxyEnable   = $($p.ProxyEnable)"
  "ProxyServer   = $($p.ProxyServer)"
  "ProxyOverride = $($p.ProxyOverride)"
  "AutoConfigURL = $($p.AutoConfigURL)"
  "--- WinHTTP 프록시 ---"
  netsh winhttp show proxy

  Section "7. 프록시 없이 직접 API 호출 (/api/tags)"
  curl.exe -s -m 10 --noproxy "*" -o NUL -w "HTTP %{http_code} (%{time_total}s)" "$Url/api/tags"; ""
  curl.exe -s -m 10 --noproxy "*" "$Url/api/tags" | Select-Object -First 1 | ForEach-Object { $_.Substring(0, [Math]::Min(300, $_.Length)) }

  Section "8. 환경 그대로 API 호출 (환경변수 프록시를 따른다)"
  curl.exe -s -m 10 -o NUL -w "HTTP %{http_code} (%{time_total}s)" "$Url/api/tags"; ""

  Section "9. 설정의 모델로 실제 질문 (/api/chat, 최대 120초)"
  $model = (Get-Content $cfgPath -Encoding UTF8 -ErrorAction SilentlyContinue | Select-String '^model\s*=\s*"([^"]+)"' | Select-Object -First 1)
  $model = if ($model) { $model.Matches[0].Groups[1].Value } else { "" }
  if ($model) {
    "모델: $model"
    $body = '{"model":"' + $model + '","messages":[{"role":"user","content":"1+1=?"}],"stream":false}'
    $tmp = Join-Path $env:TEMP "llm-check-body.json"
    [IO.File]::WriteAllText($tmp, $body)
    curl.exe -s -m 120 --noproxy "*" -w "`nHTTP %{http_code} (%{time_total}s)" -H "Content-Type: application/json" --data-binary "@$tmp" "$Url/api/chat" | ForEach-Object { $_.Substring(0, [Math]::Min(400, $_.Length)) }
    Remove-Item $tmp -ErrorAction SilentlyContinue
  } else { "설정에서 모델 이름을 못 찾음" }

  Section "10. 보안 프로그램 (네트워크를 통제하는 것이 있으면 sqlstudio.exe 가 막힐 수 있다)"
  Get-Process | Where-Object { $_.ProcessName -match '^(v3|asd)|ahnlab|nac|drm|fasoo|markany|softcamp|genian|netclient|pcfilter|alyac|safepc|dlp|endpoint|crowdstrike|sentinel|symantec|mcafee|trendmicro|tmbmsrv' } |
    Select-Object -ExpandProperty ProcessName -Unique
  "(목록에 없는 보안 제품도 있을 수 있다)"

  Section "11. SQLStudio 버전"
  $u = Get-ChildItem "HKCU:\Software\Microsoft\Windows\CurrentVersion\Uninstall", "HKLM:\Software\Microsoft\Windows\CurrentVersion\Uninstall" -ErrorAction SilentlyContinue |
    ForEach-Object { Get-ItemProperty $_.PSPath } | Where-Object { $_.DisplayName -like "SQLStudio*" } | Select-Object -First 1
  if ($u) { "$($u.DisplayName) $($u.DisplayVersion)  $($u.InstallLocation)" } else { "설치 정보를 못 찾음 (설치 파일로 설치하지 않았거나 다른 사용자로 설치)" }
} *>&1 | Tee-Object -FilePath $out

"`n결과 파일: $out"
