# Hwahap

**Hwahap 0.1.1**은 구현 계획, 코드 변경, 테스트, draft PR 검토를 연결하는 Codex plugin입니다.
스킬과 로컬 MCP 서버가 함께 설치됩니다. 모델이 계획을 정리하면 사용자가 확정하고,
이후 구현과 검증을 진행합니다. 기존 승인 계획은 원문·참조를 보존해 실행합니다.
작업별 역량·의존관계·추론 깊이·위험을 평가하고, 현재 호스트에서 가능한 모델과 effort를 선택합니다.
총비용을 줄이도록 승인 계획을 재사용하고, CI 실패를 먼저 처리하며 필요한 검토와 검증만 수행합니다.
Codex Desktop의 Plan·Goal은 선택적 참조로 연결하고 호스트가 목표 진행을 관리합니다.

## 설치

필요한 것: plugin 명령을 지원하는 Codex CLI/desktop, Git, 인증된
[GitHub CLI](https://cli.github.com/manual/gh_auth_login), 카탈로그의 모델·effort와 native agent를
실행할 수 있는 Codex 환경입니다. 설치·MCP 연결은 Codex CLI **0.153.4**에서 검증했으며,
배포 CI는 **0.152.1**을 사용합니다.
macOS 15 이상(Apple Silicon/Intel), Linux x86_64(glibc 2.39 이상)용 바이너리를 제공합니다.

```sh
codex plugin marketplace add Palbahngmiyine/hwahap --ref v0.1.1
codex plugin add hwahap@hwahap
```

Codex에서 **새 작업**을 열고 Hwahap 스킬을 선택하거나 `$hwahap`을 호출하세요.
MCP는 plugin과 함께 등록됩니다.
첫 실행에 약 1분이 걸릴 수 있습니다. 해당 버전의 런타임을 GitHub Releases에서 내려받아
SHA-256과 실행 버전을 확인하며, 이후에는 로컬 캐시를 사용합니다.

## 사용 예시

- `$hwahap 이 기능의 구현 계획만 세워줘.`
- `$hwahap 이 버그를 수정하고 테스트와 draft PR 검토까지 진행해줘.`
- `$hwahap 확정한 계획을 구현해줘.`

실제 Git 저장소에서 시작하세요. PLAN 단계에서는 질문에 답하고 표시된 `CONFIRM PLAN ...`을
직접 입력합니다. BUILD는 코드·테스트·commit·draft PR을 만들 수 있습니다.
완료한 draft를 ready로 바꾸려면 표시된 `SHIP ...`을 직접 입력합니다. PR 병합은 코드 소유자가 진행합니다.
MCP 프로세스는 로컬에서 동작하며, 사용 중인 Codex 모델과 GitHub에는 작업에 필요한 정보가 전달됩니다.

## 확인·업데이트·문제 해결

```sh
codex plugin list
codex mcp list
```

다음 버전으로 올릴 때는 marketplace ref를 해당 릴리스 태그로 다시 지정하고 plugin을 재설치한 뒤
새 작업을 시작합니다. 예: `codex plugin marketplace add Palbahngmiyine/hwahap --ref v0.1.1`.
기존 standalone Hwahap을 설치했다면 기존 MCP 등록과 전역 스킬을 정리해 중복 로딩을 피하세요.

- 다운로드 실패: 네트워크와 해당 버전의 [Release](https://github.com/Palbahngmiyine/hwahap/releases)를 확인한 뒤 재시작합니다.
- 오프라인 설치: [릴리스·설치 가이드](RELEASING.md)의 바이너리 포함 archive를 사용합니다.
- GitHub 인증 실패: `gh auth status`로 확인합니다.
- 모델·native agent 설정: [모델 카탈로그와 호스트 가용성](plugins/hwahap/USAGE.md#모델-카탈로그와-작업-평가)을 확인합니다.

[운영 절차](plugins/hwahap/OPERATIONS.md) · [상세 구조](plugins/hwahap/ARCHITECTURE.md) ·
[검증 범위](plugins/hwahap/PLATFORM.md) · [원본 이력](MIGRATION.md)

패키지 버전은 `0.1.1`이며, 내부 실행 기록은 `hwahap/v5`를 사용합니다. v4 실행은 0.1.0 런타임으로 이어가며,
[업그레이드 절차](RELEASING.md#011-업그레이드)를 따라 새 실행을 시작합니다.
GitHub repo marketplace로 배포합니다.
설치 구조는 [공식 OpenAI plugin 문서](https://developers.openai.com/plugins/build/plugins)를 따릅니다.
