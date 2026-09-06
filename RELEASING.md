# 릴리스와 오프라인 설치

## 유지보수자: 태그로 배포

[GitHub Actions](https://docs.github.com/actions)와 공식 [GitHub CLI](https://cli.github.com/manual/gh_release_create)를
사용한다. 버전 파일을 수정한 커밋에 `vX.Y.Z` 태그를 push하면 전체 CI, 플랫폼별 release 빌드,
압축 파일 검증, 실제 Codex plugin 설치·MCP 연결을 실행한다. 전부 통과한 뒤에만 GitHub Release를 공개한다.
기본 `GITHUB_TOKEN`을 사용하며 Actions의 PR 생성·승인 권한이나 별도 secret은 필요 없다.

```sh
python3 scripts/set-version.py 0.1.0
python3 tests/versions.py
cargo test --locked --manifest-path plugins/hwahap/runtime/Cargo.toml --all-targets
git add plugins/hwahap/version.txt plugins/hwahap/.codex-plugin/plugin.json plugins/hwahap/runtime/Cargo.toml plugins/hwahap/runtime/Cargo.lock README.md plugins/hwahap/README.md
git commit -m 'chore: release 0.1.0'
git tag v0.1.0
git push origin main v0.1.0
```

다음 버전에는 위 명령의 버전을 바꾼다. `fix`는 patch, `feat`는 minor,
호환되지 않는 공개 인터페이스 변경은 major 증가를 검토한다. 내부 `hwahap/v4` 기록 형식은
패키지 버전과 별개다. 기존 commit의 `Release-As` footer는 사용하지 않는다.
실패 시 Actions에서 실패한 job을 재실행하거나 Release의 수동 실행에 기존 tag를 입력한다.
공개된 릴리스의 파일은 덮어쓰지 않는다. 고칠 내용은 새 버전으로 배포한다.

## 사용자: 바이너리 포함 archive로 설치

[Releases](https://github.com/Palbahngmiyine/hwahap/releases)에서 해당 플랫폼의 `.tar.gz`와
같은 이름의 `.sha256` 파일을 같은 디렉터리에 내려받는다. macOS Apple Silicon 예시:

```sh
shasum -a 256 -c hwahap-v0.1.0-aarch64-apple-darwin.tar.gz.sha256
tar -xzf hwahap-v0.1.0-aarch64-apple-darwin.tar.gz
codex plugin marketplace add ./hwahap
codex plugin add hwahap@hwahap
```

archive에는 marketplace, plugin, 스킬, 소스와 빌드된 바이너리가 포함된다.
같은 이름의 marketplace를 이미 사용 중이라면 기존 설치의 경로를 먼저 확인한다.
plugin을 설치한 뒤 새 Codex 작업을 시작한다. 이 archive는 첫 실행 다운로드가 필요 없다.
`.gz` 파일은 plugin 런처의 자동 다운로드에 쓰는 단일 실행 파일이다.
macOS 바이너리는 서명·공증하지 않는다. Linux는 Ubuntu 24.04의 glibc를 기준으로 빌드한다.

## 개발자: 소스 빌드

```sh
cargo build --locked --release --manifest-path plugins/hwahap/runtime/Cargo.toml
plugins/hwahap/bin/hwahap --version
codex plugin marketplace add .
codex plugin add hwahap@hwahap
```

Rust 1.90 이상이 필요하다. 개발 중에는 실제 Codex 설치 검증을
`python3 tests/plugin-install.py plugins/hwahap/runtime/target/release/hwahap`으로 실행한다.
이 테스트는 임시 Codex 홈을 사용하며 개인 설정을 바꾸지 않는다.
