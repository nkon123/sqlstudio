# 릴리스와 배포

## 1. 버전 올리기

네 곳을 같은 버전으로 바꾼다.

- `app/src-tauri/tauri.conf.json` — `version`
- `app/src-tauri/Cargo.toml` — `version`
- `app/package.json` — `version`
- `app/package-lock.json` — 맨 위 두 곳의 `version`

## 2. 태그를 올리면 설치 파일이 만들어진다

```bash
git tag v0.2.0
git push origin v0.2.0
```

`.github/workflows/release.yml` 이 Windows 러너에서 다음을 한다.

1. 태그와 `tauri.conf.json` 버전이 같은지 확인한다 (다르면 멈춘다).
2. 테스트를 돌린다 (DB 없이).
3. `sqlstudio-mcp.exe`, `sqlstudio-analyze.exe` 를 만든다 (설치 파일에 함께 들어간다).
4. `npx tauri build` 를 실행한다.
   - 먼저 `npm run build` 가 돈다.
   - 그 안에서 라이선스 고지(`app/public/third-party-notices.html`)를 다시 만들고, 타입을 검사하고, 화면을 빌드한다.
   - 결과는 NSIS 설치 파일이다 (WebView2 포함, 오프라인 설치).
5. 인증서가 등록되어 있으면 서명한다 (아래 3절).
6. `SHA256SUMS.txt` 를 만들고 **초안(draft) 릴리스**에 붙인다.

공개는 GitHub 릴리스 화면에서 사람이 확인한 뒤 **Publish** 를 누른다. 수동 실행(Actions → release → Run workflow)은 설치 파일을 아티팩트로만 남긴다.

## 3. 코드 서명

서명이 없으면 Windows SmartScreen 이 "알 수 없는 게시자" 경고를 띄운다. 사내 배포 시스템(SCCM·Intune 등)은 서명을 요구하는 경우가 많다.

### 회사 코드 서명 인증서(.pfx)가 있을 때

저장소 Settings → Secrets → Actions 에 두 값을 넣는다.

- `WINDOWS_CERT_PFX` — pfx 파일을 base64 로 바꾼 값
  - 바꾸는 명령: `[Convert]::ToBase64String([IO.File]::ReadAllBytes("cert.pfx"))`
- `WINDOWS_CERT_PASSWORD` — pfx 비밀번호

워크플로가 `signtool` 로 설치 파일에 서명하고 타임스탬프를 붙인다. 현재는 설치 파일(setup.exe)만 서명한다. 설치되는 실행 파일까지 서명하려면 Tauri 의 `bundle.windows.signCommand` 를 쓴다.

### 인증서를 외부에 둘 수 없을 때 (사내 HSM·서명 서버)

GitHub 에서 서명하지 않고 아래 순서로 한다.

1. 초안 릴리스의 설치 파일을 내려받는다.
2. 사내 서명 절차로 서명한다.
3. 서명한 파일을 다시 올린다.
4. `SHA256SUMS.txt` 를 새로 만든다.

### 사내 배포만 할 때

사내 배포 시스템이 자체 서명이나 허용 목록(AppLocker·WDAC)을 쓰면 그 절차를 따른다.

## 4. 사내 배포 메모

- **조용히 설치:** `SQLStudio_x.y.z_x64-setup.exe /S`
  - 현재 사용자 기준으로 설치된다.
  - 기존 설치가 있으면 덮어 올린다.
- **필요한 것:** Oracle Instant Client 19 (64비트) 가 `PATH` 에 있어야 한다. 설치 파일에는 들어 있지 않다.
- **TNS 별칭:** 설정의 접속 화면에서 tnsnames.ora 폴더를 지정하거나, `TNS_ADMIN` 환경변수를 쓴다.
- **설정 위치:** `%APPDATA%\sqlstudio\` 아래에 있다.
  - `config.toml` — 설정
  - `history\` — SQL 실행 이력
  - `snippets.json` — 코드 조각
  - `analysis\` — 분석 결과
  - 비밀번호·API 키는 Windows 자격 증명 관리자에 들어간다.
