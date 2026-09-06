# 릴리스와 오프라인 설치

## 유지보수자: 태그로 배포

[GitHub Actions](https://docs.github.com/actions)와 공식 [GitHub CLI](https://cli.github.com/manual/gh_release_create)를
사용한다. 버전 파일을 수정한 커밋에 `vX.Y.Z` 태그를 push하면 전체 CI, 플랫폼별 release 빌드,
압축 파일 검증, 실제 Codex plugin 설치·MCP 연결을 실행한다. 전부 통과한 뒤에만 GitHub Release를 공개한다.
기본 `GITHUB_TOKEN`을 사용하고 게시 job에 `contents: write` 권한을 부여한다.

```sh
python3 scripts/set-version.py 0.1.1
python3 tests/versions.py
cargo test --locked --manifest-path plugins/hwahap/runtime/Cargo.toml --all-targets
git add plugins/hwahap/version.txt plugins/hwahap/.codex-plugin/plugin.json plugins/hwahap/runtime/Cargo.toml plugins/hwahap/runtime/Cargo.lock README.md plugins/hwahap/README.md
git commit -m 'chore: release 0.1.1'
git tag v0.1.1
git push origin main v0.1.1
```

다음 버전에는 위 명령의 버전을 바꾼다. `fix`는 patch, `feat`는 minor,
호환되지 않는 공개 인터페이스 변경은 major 증가를 검토한다. 내부 `hwahap/v5` 기록 형식은
패키지 버전과 별개이며, 배포 버전은 태그로 지정한다.
게시 실패 시 Actions에서 실패한 job을 재실행한다. Release의 수동 실행은 지정한 ref를
빌드·검증하고 Actions artifact를 만든다.
공개된 릴리스는 고정된 파일을 유지하며 수정은 새 버전으로 배포한다.

## 사용자: 바이너리 포함 archive로 설치

[Releases](https://github.com/Palbahngmiyine/hwahap/releases)에서 해당 플랫폼의 `.tar.gz`와
같은 이름의 `.sha256` 파일을 같은 디렉터리에 내려받는다. macOS Apple Silicon 예시:

```sh
shasum -a 256 -c hwahap-v0.1.1-aarch64-apple-darwin.tar.gz.sha256
tar -xzf hwahap-v0.1.1-aarch64-apple-darwin.tar.gz
codex plugin marketplace add ./hwahap
codex plugin add hwahap@hwahap
```

archive에는 marketplace, plugin, 스킬, 소스와 빌드된 바이너리가 포함된다.
같은 이름의 marketplace를 이미 사용 중이라면 기존 설치의 경로를 먼저 확인한다.
plugin을 설치한 뒤 새 Codex 작업을 시작한다. 이 archive는 포함된 바이너리로 실행한다.
`.gz` 파일은 plugin 런처의 자동 다운로드에 쓰는 단일 실행 파일이다.
Linux는 Ubuntu 24.04의 glibc를 기준으로 빌드한다.

## 개발자: 소스 빌드

```sh
cargo build --locked --release --manifest-path plugins/hwahap/runtime/Cargo.toml
plugins/hwahap/bin/hwahap --version
codex plugin marketplace add .
codex plugin add hwahap@hwahap
```

Rust 1.90 이상이 필요하다. 개발 중에는 실제 Codex 설치 검증을
`python3 tests/plugin-install.py plugins/hwahap/runtime/target/release/hwahap`으로 실행한다.
이 테스트는 임시 Codex 홈에서 설치와 연결을 확인한다.
패키지는 `tests/package.sh RUST_TARGET [SNAPSHOT_REF]`로 검증한다. 기본값은 `HEAD`이며,
작성 중인 후보는 체크포인트 ref를 지정해 소스·문서·바이너리 버전을 함께 검증한다.

## 0.1.1 업그레이드

0.1.1의 실행 기록은 `hwahap/v5`이며 0.1.0은 `hwahap/v4`를 사용한다.

1. 진행 중인 v4 작업은 기존 0.1.0 plugin·바이너리로 완료한다.
2. 보존할 작업은 관련 에이전트·명령의 종료를 확인하고 checkout과 `.hwahap` 전체를 함께 보관한다.
3. 별도 checkout에서 0.1.1 plugin을 설치하고 새 작업·run을 시작한다.
4. 필요한 사용자 승인·계획은 원문과 출처를 `approved_plan`으로 전달해 독립 변환 검토를 받는다.

기존 기록은 원래 schema와 실행 버전으로 보존한다. 새 런타임은 v4를 만나면 버전 안내를 반환한다.
카탈로그는 새 run에서 고정되며 진행 중 run의 모델·effort·역할 배정을 유지한다.
[변경 기록](plugins/hwahap/CHANGELOG.md)과 [사용성 검증](docs/plans/usability-0.1.1.md)을 참고한다.
