# Native 실행과 검증 기록

소스·자동 테스트·호스트 실행 기록을 정리한다. 기록마다 실행 버전과 관측 출처를 표시한다.

## 1. Codex 기본 하위 에이전트

[OpenAI 공식 문서](https://learn.chatgpt.com/docs/agent-configuration/subagents)는 Codex의
하위 에이전트 생성·대기·중단과 부모 권한 정책 상속을 설명한다.
Hwahap은 호스트에 노출된 native 도구로 모델 세션을 실행한다.

관찰한 `spawn_agent` 인수는 `task_name`, `message`, `fork_turns`, `model`, `reasoning_effort`다.
새 자식은 `fork_turns=none`으로 생성하고, 유지된 자식은 follow-up으로 재사용한다.
절대 작업 경로와 접근 범위는 지시에 담으며, 검토 전후 Git 상태를 검사한다.
요청 모델·effort는 실행 요청 기록에서, 호스트의 권한 정책은 호스트 설정에서 확인한다.

0.1.1은 [catalog](runtime/src/catalog.rs)의 역량·effort 정책과 현재 호스트 관찰로 모델을 선택한다.
TaskAssessment는 작업 의존관계, 공유 상태, 추론 깊이와 세 위험 값을 추가한다.
Worker·Critic·Auditor는 run과 부모별로 identity·역할·모델·effort를 유지한다.
작성 전 독립 검토자 두 명의 모델·effort·슬롯을 확인한다. 부모 작성은 검토자 두 슬롯,
새 Worker 작성은 Worker와 두 검토자 슬롯을 확보한다. 기존 작업자는 재사용한다.

## 2. 저장과 중단 복구

[호스트 실행기](runtime/src/native/host.rs)는 OS 파일 잠금으로 저장소당 실행 하나를 관리한다.
`status`는 진행 상황을 읽는다.

| 기록 | 의미 |
|---|---|
| `native-request-<id>.json` | 호스트에 전달하기 전에 저장한 실행 요청 |
| `native-pending.json` | 현재 요청, 등록된 agent ID와 완료 상태 |
| `native-owner.json` | run ID와 부모 scope |
| `pr-review.json` | PR·head·계약 결속, round·stage·누적 수정 횟수 |
| `pr-<binding>-<round>-<team>.json` | 고정된 공격·방어 보고서와 receipt 또는 예정 수정 commit |
| `native-completion-<id>.json` | 종료 결과와 선택적 사용량 |
| `native-stopped-<id>.json` | 남은 에이전트와 명령의 종료 확인 |
| `native-failure-<id>.json` | spawn 실패와 생성 여부의 호스트 관찰 |
| `native-resume-<id>.json` | 재개에 사용한 새 호스트 회복 관찰 |
| `native-timing-<id>.json` | 시각·역할·예산·입출력 bytes·최초 종료 사유 |
| `.hwahap/native-pool-<scope digest>.json` | run·부모별 작업자 정보 |
| `receipt-<sequence>-<role>.json` | 증거 출처를 표시한 세션 결과 |

호스트는 생성 직후 agent ID를 등록한다. 완료는 등록된 ID에 결속하고 동일 완료는 한 번 소비한다.
생성과 등록 사이에 연결이 끊기면 해당 요청의 에이전트와 명령을 찾아 종료를 확인한다.
`all_work_stopped=true`는 모든 관련 작업의 종료를 확인한 뒤 전달한다.

기본 한도는 run당 요청 64회, 연결 제한 180초와 등록 후 수행 제한 180초다. 재시도·follow-up도 요청 수에 포함한다.
역할별 soft 목표는 60/90/120초이며 hard 값으로 상한을 둔다. 호스트는 최대 30초 이벤트 대기를 사용한다.
종료 사유는 `completed`·`handoff_deadline`·`deadline`·`channel_closed`·`spawn_failed`·`spawn_unknown`·`stopped`이며 최초 값을 보존한다.
연결 대기와 등록 후 수행 시간을 각각 관측한다.

생성된 자식이 없는 spawn 실패는 `native_paused`에 저장하고 새 호스트 회복 관찰을 기다린다.
재개 관찰은 해당 재시도에 한 번 사용하며 새 요청을 실행 예산에 포함한다.
자식 생성 여부가 불명확하거나 작업이 중단되면 `native_stop`에서 해당 dispatch의 종료를 확인한다.
복구는 저장된 run 단계에서 진행하며 진행 중이던 unit이나 역할을 다시 실행할 수 있다.
완료된 계획 검토는 현재 `review_digest`에 결속된 결과를 재사용한다.
근거는 [config.rs](runtime/src/config.rs), [broker](runtime/src/native/broker.rs),
[failure.rs](runtime/src/native/failure.rs), [native_surface.rs](runtime/tests/native_surface.rs)다.

## 3. 자동 테스트와 호스트 관찰

자동 테스트는 임시 Git 저장소와 통제한 에이전트 결과를 사용한다.

- 단위·사이클: 모델·역할 배정, JSON 계약, 상태 저장, 첫 구현과 재작업, 검증·commit·ship 조건.
- native 인터페이스: 등록·완료 결속, 중복 처리, 재시작 복구, timeout과 잠금.
- [capacity](runtime/tests/native_capacity.rs): 실패 주입, no-child 일시 중단, 새 관찰로 재개, unknown-child 종료 확인.
- [pool](runtime/tests/native_pool.rs): 세 슬롯에서 300개 작업을 생성 3회·follow-up 297회로 처리하고 작업자 결속을 검사.
- [timing](runtime/src/native/timing.rs): 최초 시각·실패 보존, 기록 오류와 시계 역행.
- [direct BUILD](runtime/tests/direct_build.rs): 기획 생략, 재전송, 실제 Git commit·원격 push, PR 수정·복구·예산·범위 검사.
- MCP 인터페이스: 세 도구의 공개 계약과 입력 검증.

2026-09-05 pool 도입 전 호스트 시험에서는 Luna FactFinder와 Astra coordinator의 요청·완료를
각각 기록하고 `deciding / await_user`에 도달했다. pool 도입 후에는 두 run의 FactFinder를
생성 1회·follow-up 1회로 실행했다. 동일 ID가 README 변경 전 `alpha-1`, 변경 후 `beta-2`를
파일 인용과 함께 반환했다. 시험 설정 `native_max_calls=1`에서 사실 수집을 마치고 종료했다.
완료 2개, pending 정리, archive 후 pool 유지, MCP 정상 종료를 확인했다.

| 요청 | 요청→등록 | 등록→결과 전달 | 요청→결과 전달 |
|---|---:|---:|---:|
| 첫 생성 | 27,084ms | 30,278ms | 57,362ms |
| 동일 ID 재사용 | 5,559ms | 52,931ms | 58,490ms |

시간에는 부모의 전달·병행 작업이 포함된다. 사용량 보고는 0개, 청구금액은 `unknown`으로 기록했다.
시험 바이너리 SHA-256은 `af9fea8ee4228bb1e33a684f5f2465048a30f608cc9118ca2a8fee24aeeb00b2`다.

### PR #12 실행 이력

2026-09-05의 [PR #12, 소스 `129f942`](https://github.com/Palbahngmiyine/harness/pull/12/commits/129f942efd336d44e1a76fc9d06544f1486b87c7)
기록이다. 실행 바이너리는 `73057f8`에서 빌드했으며 SHA-256은
`fd9936102ae05251a9b6ec0fa93073d5abad88c48f35f26e88c4deae32c6ec7d`다.

- run `2026-09-05-improve-hwahap-build-recovery-and-eb539bc6`: direct BUILD unit 10개 accepted, review round 6 complete, `awaiting_adjust_or_ship`.
- native 요청 42개: 완료 40개(부모 17개·하위 에이전트 23개), deadline 1개, 종료 확인 후 복구 1개.
- 자식 생성 2회와 동일 ID 재사용 21회. 완료 요청의 호스트 관측 시간 중앙값 89.6초, 최대 175.8초.
- 사용량 보고 0개, 청구금액 `unknown`.
- 같은 PR·head·계약에 결속된 두 Astra 보고서에서 여섯 보안 영역을 검사. 출력 수집 결함 수정 후 최종 finding 0개.
- [CI](https://github.com/Palbahngmiyine/harness/actions/runs/33968756004): Linux 테스트 755개, Linux/macOS/Windows 작업과 gates/verify 통과.
  [CodeQL](https://github.com/Palbahngmiyine/harness/actions/runs/33968753968) 통과.

## 4. 설치와 실행 환경

[plugin 설치 안내](README.md)에 따라 설치한다. [bin/hwahap](bin/hwahap)은 포함된 바이너리나
버전별 캐시를 실행하며, 첫 실행에 해당 버전의 GitHub Release를 다운로드한다.
진단은 stderr, MCP 전송은 stdout을 사용한다. 테스트 명령은 POSIX `sh -c`, native 잠금은 Unix `flock`을 사용한다.
배포 바이너리는 macOS Apple Silicon·Intel과 Linux x86_64용이다.

## 5. 사용량과 비용

[cost.rs](runtime/src/cost.rs)는 현재 실행의 요청·완료 artifacts를 집계한다.
재시도·진행 중·중단 요청을 포함하며, requested model별 토큰과 보고 비율은 호스트의 계수를 사용한다.
사용량 미계측은 `unknown`으로 표시한다. 로컬 세션 관측과 가격표 기반 추정은 [USAGE.md](USAGE.md)를 따른다.
손상된 JSON, cached input이 total input보다 큰 값, 정수 overflow는 오류로 처리한다.
