# Hwahap 0.1.0

Codex에서 구현 계획, 코드 변경, 테스트, draft PR 검토를 진행하는 plugin입니다.
스킬과 로컬 MCP 서버를 함께 제공합니다. 설치 후 새 작업에서 `$hwahap`을 호출하세요.

[설치·사용 안내](https://github.com/Palbahngmiyine/hwahap#설치) ·
[운영 절차](OPERATIONS.md) · [상세 구조](ARCHITECTURE.md) · [MCP 사용법](USAGE.md)

첫 실행 시 GitHub Releases에서 이 패키지와 같은 버전의 런타임을 다운로드합니다.
다운로드는 SHA-256과 실행 버전을 검사한 뒤 버전별 캐시에 저장합니다.
`PLUGIN_DATA`가 있으면 그 아래에, 없으면 `${XDG_CACHE_HOME:-$HOME/.cache}/hwahap`에 저장합니다.
`HWAHAP_OFFLINE=1`이면 자동 다운로드를 하지 않습니다.
