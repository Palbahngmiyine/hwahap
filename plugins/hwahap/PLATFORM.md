# Native 실행의 플랫폼 근거와 검증 범위

2026-09-05의 현재 소스와 자동 테스트를 기준으로 작성했다. 실제 호스트의 도구 제공 여부와
모델 적용·권한·청구 사용량은 각각 확인해야 하며, 요청 기록만으로 검증됐다고 판단하지 않는다.

## 1. Codex 기본 하위 에이전트

[OpenAI 공식 문서](https://learn.chatgpt.com/docs/agent-configuration/subagents)는 Codex가
하위 에이전트 생성·대기·중단을 관리하고, 하위 에이전트가 부모의 현재 권한 정책을 상속한다고 설명한다.
Hwahap은 호스트에 노출된 native 도구를 사용하며 MCP 서버 자체가 모델 세션을 시작하지 않는다.

현재 호스트의 `spawn_agent` 호출에는 `task_name`, `message`, `fork_turns`, `model`,
`reasoning_effort`가 있다. 별도 `cwd`·`sandbox` 인수는 없다. Hwahap은 새 자식에만 `fork_turns=none`을
사용하고, 유지된 자식은 follow-up으로 재사용한다. 요청 모델·effort, 절대 경로와 접근 범위는 지시로 전달한다.
따라서 read-only 지침과 Git 사후 검사는 OS 격리의 증거가 아니다. 공식 문서의 custom agent
설정 기능도 이 구현이 개별 spawn의 샌드박스를 검증했다는 뜻은 아니다.

실행 증거는 [native 모듈](runtime/src/native.rs)과
[MCP instructions](runtime/src/mcp.rs)에 정의되어 있다.
요청 모델은 실제 적용 모델과 구분하며, 도구가 없는 호스트에서는 실행 한계를 보고한다.
부모 Astra가 추천·합성·충돌 재계획·재작업을 수행하고, Worker Luna·Critic Astra·Auditor Astra를 유지한다.
Auditor는 ColdConsumer·최종 리뷰를 맡는다. 작성에 참여하지 않지만 이전 검토 문맥은 남을 수 있다.

공식 문서는 플랫폼 기능의 근거이며, 이 저장소의 기본 프로필은
[profile.rs](runtime/src/profile.rs)가 정의한다. Deep은 `gpt-6-astra` / `high`,
Economy는 Luna / `medium`, Critic은 Astra / `high`다. direct BUILD는 Economy도 부모 Astra로 고정한다.

[공식 설정 문서](https://learn.chatgpt.com/docs/config-file/config-reference)의
`agents.max_concurrent_threads_per_session`은 부모를 제외한 동시에 열린 하위 에이전트 스레드 한도다.
미설정 시 Codex가 기본값을 정하며 `agents.max_threads`는 이전 이름이다. 이는 실행 완료 횟수의 한도가 아니다.
공식 subagents 문서에는 스레드 닫기가 설명되어 있지만, 2026-09-05 이 작업에 노출된 collaboration 도구에는
작업 중단용 `interrupt_agent`는 있고 close/release는 없다. 완료·중단은 스레드 해제의 증거가 아니며 해제 여부는 `unknown`이다.
Hwahap은 매 완료 뒤 자식을 해제하지 않는다. 일반 PLAN은 자식 세 ID, direct BUILD는
작성자를 제외한 검토자 두 ID를 유지한다. 이 최초 슬롯은 여전히 필요하다.
같은 저장소·부모 `host_session_id`만 pool을 공유한다. 다른 저장소·부모는 별도 pool이며,
기존 모델·effort 변경과 Worker·Critic·Auditor 사이의 전환·교체 생성을 거부한다. 근거는 [pool.rs](runtime/src/native/pool.rs)다.
Hwahap은 외부 capacity 회복을 보장하지 않으며 전역 한도 변경이나 무관한 작업 종료로 우회하지 않는다.

## 2. 저장과 중단 복구

[호스트 실행기](runtime/src/native/host.rs)는 저장소당 한 실행을 관리하고 OS 파일 잠금으로 다른
MCP 프로세스의 동시 실행을 거부한다. `status`는 진행 상황을 읽으며 새 에이전트를 만들지 않는다.

| 기록 | 의미 |
|---|---|
| `native-request-<id>.json` | 호스트에 전달하기 전에 저장한 실행 요청 |
| `native-pending.json` | 현재 요청과 등록된 agent ID, 완료 상태 |
| `native-owner.json` | pending 유무와 무관하게 유지되는 run ID·부모 scope |
| `pr-review.json` | PR·head·계약 binding, round·stage·누적 수정 횟수 |
| `pr-<binding>-<round>-<team>.json` | 교체 불가한 공격·방어 보고서와 receipt 또는 예정 수정 commit |
| `native-completion-<id>.json` | 호스트가 전달한 종료 결과와 선택적 사용량 |
| `native-stopped-<id>.json` | 호스트가 남은 에이전트와 명령의 종료를 확인한 기록 |
| `native-failure-<id>.json` | 정확한 spawn 실패와 생성 여부에 대한 호스트 관찰 |
| `native-resume-<id>.json` | 해당 중단을 재개하는 새 호스트 회복 관찰 |
| `native-timing-<id>.json` | 요청·등록·종료 시각, 역할·예산, 입력·출력 bytes와 최초 종료 사유 |
| `.hwahap/native-pool-<scope digest>.json` | 부모별 작업자 ID·역할·모델·effort; artifacts 밖에 있어 run archive 후에도 유지 |
| `receipt-<sequence>-<role>.json` | 실행 증거의 출처가 명시된 세션 결과 |

호스트는 생성 직후 agent ID를 등록하고, 동일 요청에 두 번째 에이전트를 만들지 않는다.
완료는 등록된 ID와 일치해야 하며 같은 내용의 재전송은 한 번만 소비한다. 다른 완료 내용은 거부한다.
생성 후 등록 전에 끊겼다면 호스트는 해당 요청으로 만든 에이전트가 있는지 찾아 종료해야 한다.
종료 여부가 불명확할 때 `all_work_stopped=true`를 보내면 안 된다.

기본 한도는 run당 native 요청 64회, 요청당 hard timeout 180초다. 재시도·follow-up도 요청 수에 포함된다.
역할별 soft 목표 60/90/120초는 hard 값으로 상한을 둔다. 분류는 [timing.rs](runtime/src/native/timing.rs)를 따른다.
호스트는 최대 30초 이벤트 대기로 완료를 확인하며, hard 만료는 기존 중단 확인 절차로 이어진다.
종료 사유는 `completed`·`deadline`·`channel_closed`·`spawn_failed`·`spawn_unknown`·`stopped`를 기록하고 최초 값을 보존한다.
관측 구간에는 호스트 queue·spawn·relay가 포함된다. 등록 이후 구간도 순수 모델 실행 시간은 아니다.
완료된 계획 검토는 현재 `review_digest`가 같은 경우에만 재사용한다. 실패 findings도 보존한다.
자식이 없다고 확인된 spawn 실패는 `native_paused`로 저장한다. 자동 재시도·polling·새 요청은 중단하고
run·plan·accepted unit을 유지한다. 새로 관찰한 호스트 회복 근거로 명시적으로 재개하며, 같은 근거를
다른 재시도에 재사용할 수 없다. 재개 후 생성되는 새 요청도 `native_max_calls`에 포함된다.
자식 생성 여부가 불명확한 실패와 미완료 작업의 재시작·timeout은 `native_stop`으로 보낸다.
이는 실제 종료를 자동 수행했다는 뜻이 아니다. 정확한 dispatch의 자식·명령을 찾아 종료를 확인해야 한다.
재개는 저장된 run 단계에서 진행하므로 아직 accepted가 아닌 unit이나 단계 내부 역할은 반복될 수 있다.
메모리에만 있던 역할 진행까지 정확히 이어받거나 모든 미완료 변경을 보존하는 계약은 아니다.
[config.rs](runtime/src/config.rs), [broker](runtime/src/native/broker.rs),
[failure.rs](runtime/src/native/failure.rs), [native_surface.rs](runtime/tests/native_surface.rs)가 근거다.

## 3. 현재 확인한 것과 남은 검증

자동 테스트는 실제 임시 Git 저장소와 통제한 에이전트 결과를 사용한다.

- 단위 테스트: 모델·역할 배정, 상태 저장, JSON 계약, 사용량 오류와 집계.
- 사이클 테스트: Luna 첫 구현 뒤 Astra 재작업 한 번, 검증·검토·commit·ship 조건.
- native 인터페이스 테스트: 등록·완료 연결, 중복 완료, 재시작 복구, 시간 초과와 잠금.
- [capacity 테스트](runtime/tests/native_capacity.rs): 통제한 실패 주입으로 no-child 중단·재시작, 새 근거 재개, 근거 재사용 거부, unknown-child 종료 확인, 실패 기록 중 단절을 검사한다.
- [pool 테스트](runtime/tests/native_pool.rs): 통제한 세 슬롯에서 300개 작업을 생성 3회·follow-up 297회로 처리하고 역할·ID·모델·effort 변경과 오래된 응답을 거부한다.
- [timing 테스트](runtime/src/native/timing.rs): 최초 시각·실패 보존, 기록 누락·손상 오류와 시계 역행을 검사한다.
- [direct BUILD 테스트](runtime/tests/direct_build.rs): 기획 생략, 초기화 재전송, 실제 Git commit·원격 push와 같은 PR 수정, 오래된 보고서·동일 검토자 거부, 방어 단절 재개·예산 보존·범위 변조 거부.
- MCP 인터페이스 테스트: 세 도구의 공개 계약과 입력 검증.

capacity 테스트는 실제 호스트 pool을 고갈시킨 실험이 아니다. 실제 스레드 해제와 슬롯 반환,
해제 후 spawn 성공은 검증하지 않았으며, 아래의 canary도 그 증거가 아니다.

세 작업자 pool 변경 전인 2026-09-05의 제한된 실제 호스트 canary에서는 Luna `fact_finder` 하위 에이전트를 생성하고,
등록한 뒤 반환된 JSON을 completion으로 전달했다. 이어 현재 Astra가 `recommender`를
`coordinator`로 등록·처리했고, 실행은 `deciding / await_user`까지 진행했다. 요청 2개, 완료 2개
(하위 에이전트 1개·coordinator 1개), 미완료 0개, 사용량 보고 0개가 기록됐다. 두 native receipt와
pending 제거를 확인했으며 EOF로 MCP가 종료됐다. 실제 사용 토큰이나 청구금액은 확인하지 못했다.
초기 canary에서 발견한 자체 상태 파일 변경 오탐은 수정하고 `.gitignore` 없는 저장소에서 재검증했다.
이 관찰은 전체 계획·구현·최종 검토·PR 생성이나 새 pool 재사용의 실제 호스트 성공 증거가 아니다.
과거 360초 소요를 설명하는 실행 타임라인은 확인하지 못했다. hard 제한 변경은 실제 시간 단축의 측정 결과가 아니다.

같은 날 pool 변경 후 실제 호스트에서 두 run의 FactFinder를 생성 1회·follow-up 1회로 실행했다.
동일 ID를 유지하고 README 변경 전 `alpha-1`, 변경 후 `beta-2`를 실제 파일 인용과 함께 반환했다.
각 run은 시험 설정 `native_max_calls=1`로 사실 수집 후 의도적으로 blocked 종료했다. 계획 승인은 생성하지 않았다.
완료 2개, pending 제거, archive 후 pool 유지, MCP 정상 종료를 확인했다. 호스트 관측 시간은 다음과 같다.

| 요청 | 요청→등록 | 등록→결과 전달 | 요청→결과 전달 |
|---|---:|---:|---:|
| 첫 생성 | 27,084ms | 30,278ms | 57,362ms |
| 동일 ID 재사용 | 5,559ms | 52,931ms | 58,490ms |

부모의 전달·병행 작업 시간도 포함되므로 이 두 표본은 속도 비교 실험이 아니다. 실제 청구금액은 unknown이다.
시험 바이너리 SHA-256: `af9fea8ee4228bb1e33a684f5f2465048a30f608cc9118ca2a8fee24aeeb00b2`.
실제 검증 범위는 FactFinder의 run 간 재사용이며 세 역할 전체의 모델 실행이나 pool 고갈·회복은 포함하지 않는다.

이번 개선의 실제 실행은 원격 main `0198e190`에서 시작했다. 병합된 버전에는 direct BUILD 진입점이 없어
최소 bootstrap 변경을 검증한 뒤 고정 바이너리로 U1부터 실행했다. bootstrap 바이너리 SHA-256은
`1ffd237e854132b87c5b33ca78f9dff862c2c404861d771c07a62634b4733db3`이다.
bootstrap 단계에서는 부모 Astra의 구현, 동일 Astra Critic의 unit 재사용, 실패·timeout 뒤 종료 확인과
run 복구를 관찰했다. bootstrap 자체는 이전의 PR 전 최종 리뷰를 수행했다. 이후 후보 바이너리로 기존
draft를 재검토해 PR 후 두 팀 검토까지 확인했다. 실행 버전과 수정 대상 코드는 구분해야 한다.

### PR #12의 과거 BUILD·보안 검토 기록

아래는 당시 바이너리의 관찰 이력이며 v4 실행 검증이 아니다. v4는 아래의 구형 보고서·PR 기록
복구 경로를 제거했다. 현재 검증과 설치는 별도 기록하며 과거 실행을 새 버전의 성공으로 재사용하지 않는다.

2026-09-05, [PR #12의 `129f942`](https://github.com/Palbahngmiyine/harness/pull/12/commits/129f942efd336d44e1a76fc9d06544f1486b87c7)
시점에 고정한 관찰이다. 이후 검토 요청 수가 늘어나도 이 표본의 범위와 수치를 바꾸지 않는다.
run은 `2026-09-05-improve-hwahap-build-recovery-and-eb539bc6`이며 direct BUILD unit 10개가 accepted,
마지막 review round 6이 complete, 상태는 `awaiting_adjust_or_ship`이었다. round 번호는 실제 팀 실행 횟수와
같지 않다. 구버전 보고서 갱신과 head 변경으로 번호가 증가할 수 있다.

- native 요청 42개 중 완료 기록 40개: 부모 17개, 하위 에이전트 23개. 최초 자식 2개와 같은 ID 재사용 21개였다.
- 나머지 요청 2개는 deadline 1개, 종료 확인 후 복구 1개다. 새 에이전트로 교체하지 않았다.
- 완료 요청의 요청→결과 전달 시간은 중앙값 89.6초, 최대 175.8초였다. timeout 요청은 180.0초에 기록됐다.
  이는 부모 구현·하위 검토를 합한 호스트 관측값이며 모델 실행만의 시간도, 전체 작업 소요시간도 아니다.
- 사용량 보고 0개, 청구금액 unknown. 이전 360초 사례와 동일 조건으로 비교한 성능·비용 실험은 아니다.
- 최종 두 Astra 보고서는 같은 PR·head·계약에 결속했고, 양쪽 여섯 보안 영역에 검사 근거가 있었다.
  확인된 출력 수집 문제 1건을 수정한 후 최종 finding은 0개였다. 취약점 부재의 보증은 아니다.
- [전체 검사 CI](https://github.com/Palbahngmiyine/harness/actions/runs/33968756004)에서 Linux 테스트 755개와
  Linux/macOS/Windows 작업, gates/verify가 통과했고 [CodeQL](https://github.com/Palbahngmiyine/harness/actions/runs/33968753968)도 통과했다.

이 보안 검토의 실행 바이너리는 `73057f8`에서 빌드했으며 SHA-256은
`fd9936102ae05251a9b6ec0fa93073d5abad88c48f35f26e88c4deae32c6ec7d`였다.
검증 대상 최종 소스는 `129f942`다. 출력 제한은 그 소스로 만든 테스트의 실제 로컬 명령 실행으로 검증했다.
Hwahap 자체 업그레이드에는 bootstrap, 고정 바이너리 교체, 호스트의 직접 수정과 검증이 포함됐다.
완성된 단일 버전만으로 처음부터 모든 변경을 자동 수행한 실험은 아니다.

| 발견 사항 | 확인 수준 | 조치와 남은 범위 |
|---|---|---|
| 작성자가 `hwahap@localhost`로 고정됨 | 실제 PR commit metadata와 `git.rs`에서 확인 | 후속 수정에서 일반 commit·commit-tree 모두 Git 사용자 설정을 사용. 공개된 과거 commit을 자동 재작성하지 않음 |
| gh 인증은 되었지만 Git push에 쓸 helper가 없었음 | 실제 push 실패와 저장소 설정 확인 | 저장소의 Git credential helper를 명시해 복구. gh 인증과 Git 작성자·push 설정은 별개이며 환경별 사전 확인 필요 |
| 결과 전달 중단 | 검증용 로컬 클라이언트의 긴 PTY 입력 잘림 및 없는 결과 파일 참조를 관찰 | 파일 경유 전달, 클라이언트 재시작, stopped 확인 후 같은 ID 재사용. 이 클라이언트 오류를 Rust 런타임 결함으로 분류하지 않음 |
| push 후 관찰한 PR head 불일치 | 실제 차단 관찰, 원인은 확정하지 못함 | 알려진 이전 head만 제한적으로 재조회하고 다른 SHA는 즉시 거부. 관련 세 경로를 회귀 검사 |
| 구버전 PR URL 보존·조회 사이 PR 교체 경계 | 독립 검토와 통제한 GitHub 응답으로 재현 | 원래 URL을 먼저 저장하고 그 URL만 갱신. 실제 PR을 닫거나 대체하는 실험은 하지 않음 |
| 명령 출력의 무제한 메모리 수집 | 양팀 소스 추적 확인과 1,025바이트 유한 회귀 검사 | 스트림별 1 MiB 제한·초과 실패·종료 처리. 실제 OOM이나 부하 공격은 수행하지 않음 |
| 보안 미확인·구버전 보고서의 재검토 | 통제한 회귀 검사에서 동일 round 재사용을 재현 | 명시적 재검토 시 새 round와 근거를 받고 이전 보고서는 보존 |

이 호스트의 전역 스킬에는 v2가 남아 있었고, v3 검증은 저장소의 스킬과 고정 Rust 바이너리를 사용했다.
PR 수정·merge만으로 설치된 스킬과 MCP 실행 파일이 갱신되지는 않는다. 설치 후 경로·버전·도구 연결을
다시 확인해야 한다. native MCP 도구가 직접 노출되지 않아 검증용 STDIO 클라이언트를 사용한 것도
이번 호스트의 운영 조건이며, 제품에 Python 의존성을 추가했다는 뜻은 아니다.

다음은 별도 실제 실행으로 검증해야 한다.

- 일반 PLAN의 계획 합성부터 구현·PR 생성까지 포함하는 전체 실제 모델 실행. 위 완료 기록은 direct BUILD다.
- 실제 명령을 실행 중인 하위 에이전트 강제 중단 이후 모든 남은 명령의 종료와 복구.
- 실제 호스트의 capacity 거부·회복과, 제공되는 경우 close/release 이후 슬롯 반환.
- 각 배포 환경에서 MCP 연결, 하위 에이전트 도구 및 요청 모델·effort의 제공 여부.
- 같은 성공 조건에서 기존 구성과 비교한 성공률·소요시간·사용량·사용자 개입.

## 4. 설치와 실행 환경

설치는 [README.md](README.md)의 plugin 설치 절차를 따른다.
[bin/hwahap](bin/hwahap)은 빌드된 바이너리를 찾아 실행하며, 진단은 stderr에 쓰고
stdout은 MCP 전송에 사용한다. 이 구현은 스킬 디렉터리 배치를 유지하며 별도 플러그인 패키징에
의존하지 않는다.

테스트 명령은 POSIX `sh -c`로 실행하고 native 실행 잠금은 Unix `flock`을 사용한다.
Windows 네이티브 실행은 지원을 확인하지 않았으며 현재 잠금 구현이 거부한다.
호스트의 권한 정책과 실제 작업 경로 접근 가능 여부도 배포 환경에서 확인해야 한다.

## 5. 사용량과 비용의 의미

[cost.rs](runtime/src/cost.rs)는 현재 실행의 요청·완료 artifacts를 집계하며 receipt를 다시 더하지
않는다. 재시도·미완료·중단 요청도 포함한다. requested model별 토큰 소계와 coordinator·하위
에이전트의 사용량 보고 비율은 호스트가 제공한 계수에 근거한다. 실제 적용 모델은 독립 검증하지 않는다.

누락된 사용량은 0이 아니라 `unknown`이다. coordinator가 보고한 작업 사용량과 부모의 결과 전달
토큰도 구분하며, 후자는 계측하지 않는다. 따라서 전체 청구금액은 `unknown`이다. 요청 수 감소나
모델 교체만으로 실제 비용 절감을 단정하지 않는다. 손상된 JSON, cached input이 total input보다
큰 값, 정수 overflow는 명시적 오류로 처리한다.
