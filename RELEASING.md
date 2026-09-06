# 릴리스와 설치

Google의 [Release Please Action](https://github.com/googleapis/release-please-action)을 사용한다.
`main`에 push하면 Linux/macOS/Windows CI가 먼저 실행된다. 모두 통과하면 Release Please가
버전과 CHANGELOG를 갱신하는 PR을 만든다. 이 PR을 병합하면 다음 실행이 `vX.Y.Z` 태그와
GitHub Release를 만들고 Linux x86_64와 macOS Apple Silicon 패키지·SHA-256 파일을 업로드한다.
Windows는 CI 검증 대상이며 POSIX 런처 배포 대상은 아니다.

- `fix: ...`: patch, `feat: ...`: minor, `feat!: ...` 또는 `BREAKING CHANGE:`: major.
- 최초 이전 커밋의 `Release-As: 4.0.0`으로 첫 릴리스는 기존 버전을 유지한다.
- 문서만 바뀐 커밋은 보통 릴리스를 만들지 않는다. 필요한 경우 Conventional Commit의 의도를 명시한다.
- `version.txt`, `runtime/Cargo.toml`, Cargo.lock의 hwahap 버전은 함께 갱신된다.
- `hwahap/v4` 계약 버전은 패키지 버전과 별개이며 자동 변경하지 않는다.

기본 `GITHUB_TOKEN`을 사용하므로 별도 secret은 필요 없다. 저장소 Actions 설정에서
**Allow GitHub Actions to create and approve pull requests**가 활성화되어야 한다.
이 토큰이 만든 PR은 일반 PR CI를 발생시키지 않으므로 Release 워크플로가 릴리스 PR의
CI를 직접 호출한다. 패키지 업로드도 별도 release 이벤트에 의존하지 않고 같은 실행에서 처리한다.
자동 병합은 설정하지 않는다. 실패하면 Actions 로그를 확인한 뒤 실패한 job을 재실행한다.

## 압축 파일 설치

[Releases](https://github.com/Palbahngmiyine/hwahap/releases)에서 해당 플랫폼의 `.tar.gz`와
같은 이름의 `.sha256` 파일을 같은 디렉터리에 내려받는다. 다음은 macOS 예시다.
파일명의 버전은 내려받은 버전으로 바꾼다.

```sh
shasum -a 256 -c hwahap-v4.0.0-aarch64-apple-darwin.tar.gz.sha256
tar -xzf hwahap-v4.0.0-aarch64-apple-darwin.tar.gz
hwahap_install="${CODEX_HOME:-$HOME/.codex}/skills/hwahap"
test ! -e "$hwahap_install"
mkdir -p "$(dirname "$hwahap_install")"
mv hwahap "$hwahap_install"
"$hwahap_install/bin/hwahap" --version
codex mcp add hwahap -- "$hwahap_install/bin/hwahap"
```

이미 설치된 경로에는 덮어쓰지 않는다. 기존 설치를 별도로 정리한 뒤 실행한다.
Linux 패키지는 Ubuntu 24.04에서 빌드한 glibc 바이너리다. 그보다 오래된 Linux나 다른
플랫폼에서는 README의 소스 빌드 절차를 사용한다. macOS 바이너리는 서명·공증하지 않는다.
설치용 압축 파일에는 스킬 문서, 런처, 소스와 해당 플랫폼의 release 바이너리가 포함된다.

설정 근거: [manifest configuration](https://github.com/googleapis/release-please/blob/main/docs/manifest-releaser.md),
[extra-file updaters](https://github.com/googleapis/release-please/blob/main/docs/customizing.md#updating-arbitrary-toml-files).
Cargo.lock 선택식의 `name.value`는 고정한 Release Please 17.3.0 TOML 파서의 표현이다.
Action을 업그레이드할 때 실제 updater로 hwahap 항목만 바뀌는지 검증한다.
